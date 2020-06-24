use std::convert::TryInto;
use std::io::{ErrorKind, Read};
use std::net::TcpStream;
use std::num::NonZeroU16;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::util::{write_to_stream_log_count, Refresh};

// Based on WebSocket RFC - https://tools.ietf.org/html/rfc6455
const FINAL_FRAGMENT: u8 = 0b1000_0000;
const BINARY_MESSAGE_OPCODE: u8 = 0b0000_0010;
const CLOSE_OPCODE: u8 = 0b0000_1000;
const PING_OPCODE: u8 = 0b0000_1001;
const PONG_OPCODE: u8 = 0b0000_1010;
const MASK_BIT: u8 = 0b1000_0000;
const MASKING_KEY_SIZE: usize = 4;

enum ReadState {
	None,
	ReadOp {
		op: u8,
	},
	ReadingKeymask {
		op: u8,
		payload_len: u8,
		keymask: Vec<u8>,
	},
	ReadingPayload {
		op: u8,
		payload_len: u8,
		keymask: Vec<u8>,
		incoming_payload: Vec<u8>,
	},
	PayloadRead {
		op: u8,
		payload: Vec<u8>,
	},
	Close,
}

pub fn handle_stream(
	mut stream: TcpStream,
	key: &str,
	cond_pair: &Arc<(Mutex<Refresh>, Condvar)>,
) {
	let mut m = sha1::Sha1::new();
	m.update(key.as_bytes());
	m.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
	let accept_value = base64::encode(m.digest().bytes());

	write_to_stream_log_count(format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\nSec-WebSocket-Protocol: chat\r\n\r\n", accept_value).as_bytes(), &mut stream);

	stream.set_nonblocking(true).expect(
		"Failed changing WebSocket TCP connection to nonblocking mode.",
	);

	let mut read_state = ReadState::None;

	let (mutex, cvar) = &**cond_pair;
	loop {
		let last_index = mutex
			.lock()
			.unwrap_or_else(|e| panic!("Failed locking mutex: {}", e))
			.index;
		let (guard, result) = cvar
			.wait_timeout_while(
				mutex
					.lock()
					.unwrap_or_else(|e| panic!("Failed locking mutex: {}", e)),
				Duration::from_millis(50),
				|pending| pending.index == last_index,
			)
			.unwrap_or_else(|e| panic!("Failed waiting: {}", e));

		if result.timed_out() {
			read_state = read_stream(&mut stream, read_state);

			if let ReadState::Close = read_state {
				return;
			}
		} else {
			let changed_file = if let Some(path) = &guard.file {
				String::from(path.to_string_lossy())
			} else {
				String::from("")
			};
			println!("Received file change notification ({}), time to notify the browser.", changed_file);

			let changed_file_len: u8 =
				changed_file.len().try_into().unwrap_or_else(|e|
					panic!("Changed file path was too long ({}) to fit into expected u8: {}", changed_file.len(), e));
			if changed_file_len > 125 {
				panic!("Don't support sending variable-length WebSocket frames yet.")
			}

			let header =
				[FINAL_FRAGMENT | BINARY_MESSAGE_OPCODE, changed_file_len];
			write_to_stream_log_count(&header, &mut stream);
			write_to_stream_log_count(changed_file.as_bytes(), &mut stream);
		}
	}
}

fn read_stream(
	mut stream: &mut TcpStream,
	mut read_state: ReadState,
) -> ReadState {
	let mut buf = [0_u8; 64 * 1024];
	let read_size = stream.read(&mut buf).unwrap_or_else(|e| match e.kind() {
		ErrorKind::WouldBlock => 0,
		_ => panic!("Failed reading: {}", e),
	});

	let mut buf_offset = 0_usize;
	loop {
		if buf_offset >= read_size {
			// Allow an additional match below even though we might have
			// reached the end of the buffer.
			if let ReadState::PayloadRead { .. } = read_state {
			} else {
				break;
			}
		}

		read_state = match read_state {
			ReadState::None => {
				let op_byte = buf[buf_offset];
				if op_byte & FINAL_FRAGMENT == 0 {
					panic!("Multi-fragment frames are not supported. Offset: {}, buffer: {:?}", buf_offset, &buf[buf_offset..usize::min(buf_offset + 128, buf.len())]);
				}
				buf_offset += 1;
				let op = op_byte & 0b0000_1111;
				ReadState::ReadOp { op }
			}
			ReadState::ReadOp { op } => {
				if buf[buf_offset] & MASK_BIT == 0 {
					panic!("Client is always supposed to set mask bit.");
				}
				let payload_len = buf[buf_offset] & !MASK_BIT;
				if payload_len > 125 {
					panic!("Server only expects control frames, which per RFC 6455 only have payloads of 125 bytes or less.");
				}
				buf_offset += 1;
				ReadState::ReadingKeymask {
					op,
					payload_len,
					keymask: Vec::new(),
				}
			}
			ReadState::ReadingKeymask {
				op,
				payload_len,
				mut keymask,
			} => {
				let keymask_end =
					buf_offset + (MASKING_KEY_SIZE - keymask.len());
				if keymask_end > read_size {
					keymask.extend_from_slice(&buf[buf_offset..]);
					buf_offset = read_size;
					ReadState::ReadingKeymask {
						op,
						payload_len,
						keymask,
					}
				} else {
					keymask.extend_from_slice(&buf[buf_offset..keymask_end]);
					buf_offset = keymask_end;
					if payload_len > 0 {
						ReadState::ReadingPayload {
							op,
							payload_len,
							keymask,
							incoming_payload: Vec::new(),
						}
					} else {
						ReadState::PayloadRead {
							op,
							payload: Vec::new(),
						}
					}
				}
			}
			ReadState::ReadingPayload {
				op,
				payload_len,
				keymask,
				mut incoming_payload,
			} => {
				let payload_end = buf_offset
					+ (usize::from(payload_len) - incoming_payload.len());
				if payload_end > read_size {
					incoming_payload.extend_from_slice(&buf[buf_offset..]);
					buf_offset = read_size;
					ReadState::ReadingPayload {
						op,
						payload_len,
						keymask,
						incoming_payload,
					}
				} else {
					incoming_payload
						.extend_from_slice(&buf[buf_offset..payload_end]);

					for i in 0..incoming_payload.len() {
						incoming_payload[i] ^= keymask[i % MASKING_KEY_SIZE];
					}

					buf_offset = payload_end;
					ReadState::PayloadRead {
						op,
						payload: incoming_payload,
					}
				}
			}
			ReadState::PayloadRead { op, payload } => {
				handle_frame(&mut stream, op, &payload)
			}
			ReadState::Close => break,
		}
	}

	read_state
}

fn handle_frame(
	mut stream: &mut TcpStream,
	op: u8,
	payload: &[u8],
) -> ReadState {
	match op {
		CLOSE_OPCODE => {
			let (status_code, message): (Option<NonZeroU16>, String) =
				if payload.len() > 1 {
					(
						NonZeroU16::new(u16::from_be_bytes([
							payload[0], payload[1],
						]))
						.or_else(|| {
							panic!("Zero status codes are not allowed according to the WebSocket RFC.")
						}),
						String::from_utf8_lossy(&payload[2..]).to_string(),
					)
				} else {
					(None, String::from(""))
				};
			println!(
				"Received WebSocket connection close, responding in kind. Payload size: {}, Status code: {:?}, message: {}", payload.len(), status_code, message
			);

			let mut return_frame = Vec::with_capacity(4);
			return_frame.push(FINAL_FRAGMENT | CLOSE_OPCODE);
			if let Some(status_code) = status_code {
				return_frame.push(2);
				return_frame
					.extend_from_slice(&status_code.get().to_be_bytes());
			} else {
				return_frame.push(0);
			};
			write_to_stream_log_count(&return_frame, &mut stream);

			ReadState::Close
		}
		PING_OPCODE => {
			println!(
				"Got PING message, responding with PONG, payload: {:?}",
				payload
			);
			let header = [
				FINAL_FRAGMENT | PONG_OPCODE,
				payload.len().try_into().unwrap_or_else(|e| {
					panic!("Unexpected payload size ({}): {}", payload.len(), e)
				}),
			];
			write_to_stream_log_count(&header, &mut stream);
			write_to_stream_log_count(payload, &mut stream);

			ReadState::None
		}
		_ => {
			println!("WARNING: Received frame with unhandled opcode: {:X}", op,);

			ReadState::None
		}
	}
}
