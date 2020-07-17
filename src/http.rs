use std::fs;
use std::io::{ErrorKind, Read};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::thread;

use crate::util;
use crate::util::{write_to_stream_log_count, Refresh};
use crate::websocket;

pub fn spawn_listening_thread(
	host: &str,
	port: i16,
	root_dir: PathBuf,
	fs_cond: Option<Arc<(Mutex<Refresh>, Condvar)>>,
	start_file: Option<PathBuf>,
) -> thread::JoinHandle<()> {
	let listener = TcpListener::bind(format!("{}:{}", host, port))
		.unwrap_or_else(|e| {
			panic!("Failed to bind TCP listening port {}:{}: {}", host, port, e)
		});
	println!("Listening for connections on http://{}:{}/dev", host, port);

	let listener_builder =
		thread::Builder::new().name("TCP_listener".to_string());
	listener_builder
		.spawn(move || {
			for stream in listener.incoming() {
				match stream {
					Ok(stream) => {
						let root_dir_clone = root_dir.clone();
						let fs_cond_pair_clone = fs_cond.clone();
						let start_file_clone = start_file.clone();
						let stream_builder = thread::Builder::new()
							.name("TCP_stream".to_string());
						stream_builder
							.spawn(move || {
								handle_client(
									stream,
									&root_dir_clone,
									&fs_cond_pair_clone,
									start_file_clone,
								)
							})
							.unwrap_or_else(|e| {
								panic!(
									"Failed spawning TCP stream thread: {}",
									e
								)
							});
					}
					Err(e) => println!("WARNING: Unable to connect: {}", e),
				}
			}
		})
		.unwrap_or_else(|e| {
			panic!("Failed spawning TCP listening thread: {}", e)
		})
}

enum ReadResult {
	GetRequest(PathBuf),
	WebSocket(String),
}

fn handle_client(
	mut stream: TcpStream,
	root_dir: &PathBuf,
	fs_cond: &Option<Arc<(Mutex<Refresh>, Condvar)>>,
	start_file: Option<PathBuf>,
) {
	let sockets_enabled = fs_cond.is_some();
	if let Some(result) = handle_read(&mut stream) {
		match result {
			ReadResult::GetRequest(path) => handle_write(
				stream,
				&path,
				root_dir,
				start_file,
				sockets_enabled,
			),
			ReadResult::WebSocket(key) => {
				if let Some(fs_cond) = fs_cond {
					websocket::handle_stream(stream, &key, fs_cond)
				}
			}
		}
	}
}

fn handle_read(stream: &mut TcpStream) -> Option<ReadResult> {
	// TODO: Read HTTP requests bigger than 4K?
	let mut buf = [0_u8; 4096];
	let size = match stream.read(&mut buf) {
		Ok(size) => size,
		Err(e) => match e.kind() {
			ErrorKind::WouldBlock => 0,
			ErrorKind::ConnectionReset => return None,
			_ => panic!("Unable to read stream: {}", e),
		},
	};

	if size == buf.len() {
		panic!("Request sizes as large as {} are not supported.", size)
	} else if size == 0 {
		// Seen this occur a few times with zero-filled buf.
		// Not sure about the cause of it.
		println!("Zero-size TCP stream read()-result. Ignoring.");
		return None;
	}

	let req_str = String::from_utf8_lossy(&buf);
	println!("Request (size: {}):\n{}", size, req_str);
	let mut lines = req_str.lines();
	let first_line = lines
		.next()
		.unwrap_or_else(|| panic!("Missing lines in HTTP request."));
	let mut components = first_line.split(' ');
	let method = components.next().unwrap_or_else(|| {
		panic!(
			"Missing components in first HTTP request line: {}",
			first_line
		)
	});
	if method != "GET" {
		panic!("Unsupported method \"{}\", line: {}", method, first_line)
	}

	let path = components
		.next()
		.unwrap_or_else(|| panic!("Missing path in: {}", first_line));

	let mut websocket_key = None;
	for line in lines {
		let mut components = line.split(' ');
		if let Some(component) = components.next() {
			if component == "Sec-WebSocket-Key:" {
				websocket_key = components.next();
			} else if component == "Sec-WebSocket-Protocol:" {
				let protocols: String = components.collect::<String>();
				panic!("We don't handle protocols correctly yet: {}", protocols)
			}
		}
	}

	if let Some(key) = websocket_key {
		return Some(ReadResult::WebSocket(key.to_string()));
	}

	if !path.starts_with('/') {
		panic!(
			"Expected path to start with leading slash, but got: {}",
			path
		)
	}
	Some(ReadResult::GetRequest(PathBuf::from(
		// Strip leading root slash.
		&path[1..],
	)))
}

const DEV_PAGE_HEADER: &[u8; 1244] = b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=UTF-8\r\n\r\n\
<html>
<head>
<title>Sitegen - Hot reload mode</title>
<link rel=\"icon\" href=\"data:;base64,iVBORw0KGgo=\">
<script>
// Tag on time in order to distinguish different sockets.
let socket = new WebSocket(\"ws://\" + window.location.hostname + \":\" + window.location.port + \"/chat?now=\" + Date.now())
socket.onopen = function(e) {
	//alert(\"[open] Connection established\")
}
socket.onmessage = function(e) {
	reader = new FileReader()
	reader.onload = () => {
		text = reader.result
		if (text == \"*\") {
			window.frames['preview'].location.reload()
		} else {
			window.frames['preview'].location.href = text
		}
	}
	reader.readAsText(e.data)
}
socket.onerror = function(e) {
	alert(`Socket error: ${e}`)
}
window.addEventListener('beforeunload', (event) => {
	socket.close()
});
</script>
<style type=\"text/css\">
BODY {
	font-family: \"Helvetica Neue\", Helvetica, Arial, sans-serif;
	margin: 0;
}
.banner {
	background: rgba(0, 0, 255, 0.4);
	position: fixed;
}
@media (prefers-color-scheme: dark) {
	BODY {
		background: black; /* Prevents white flash on Firefox. */
		color: white;
	}
}
</style>
</head>
<body>
<div class=\"banner\">Preview, save Markdown file to disk for live reload:</div>
";
const DEV_PAGE_FOOTER: &[u8; 17] = b"</body>\n</html>\r\n";

fn handle_write(
	mut stream: TcpStream,
	path: &PathBuf,
	root_dir: &PathBuf,
	start_file: Option<PathBuf>,
	sockets_enabled: bool,
) {
	const TEXT_OUTPUT_EXTENSIONS: [&str; 4] = [
		util::ASCII_EXTENSION,
		util::CSS_EXTENSION,
		util::HTML_EXTENSION,
		util::XML_EXTENSION,
	];
	const IMAGE_OUTPUT_EXTENSIONS: [&str; 3] = [
		util::GIF_EXTENSION,
		util::JPG_EXTENSION,
		util::PNG_EXTENSION,
	];

	if path.to_string_lossy() == "dev" {
		if sockets_enabled {
			println!("Requested dev hot-reload path.");
			let iframe_src = if let Some(path) = start_file {
				let mut s = String::from(" src=\"");
				s.push_str(&path.to_string_lossy());
				s.push_str("\"");
				s
			} else {
				String::from("")
			};

			write_to_stream_log_count(DEV_PAGE_HEADER, &mut stream);
			write_to_stream_log_count(format!("<iframe name=\"preview\"{} style=\"border: 0; margin: 0; width: 100%; height: 100%\"></iframe>\n", iframe_src).as_bytes(), &mut stream);
			write_to_stream_log_count(DEV_PAGE_FOOTER, &mut stream);
		} else {
			let redirect = if let Some(path) = start_file {
				path
			} else {
				PathBuf::from("/")
			};
			println!("Requested dev hot-reload path but sockets are not enabled, redirecting to: {}", redirect.display());
			write_to_stream_log_count(
				format!(
					"HTTP/1.1 302 Found\r\nLocation: {}\r\n",
					redirect.display()
				)
				.as_bytes(),
				&mut stream,
			)
		}
		return;
	}

	let mut full_path = root_dir.join(&path);
	if !full_path.is_file() {
		let with_index = full_path.join("index.html");
		if with_index.is_file() {
			full_path = with_index;
		}
	}

	println!("Attempting to open: {}", full_path.display());
	let mut input_file = match fs::File::open(&full_path) {
		Ok(input) => input,
		Err(e) => {
			match e.kind() {
				ErrorKind::NotFound => write_to_stream_log_count(
					format!("HTTP/1.1 404 Not found\r\nContent-Type: text/html; charset=UTF-8\r\n\r\n<html><body>Couldn't find: {}</body></html>\r\n", full_path.display()).as_bytes(),
					&mut stream,
				),
				_ => write_to_stream_log_count(
					format!("HTTP/1.1 500 Error\r\n{}", e)
						.as_bytes(),
					&mut stream,
				)
			}
			return;
		}
	};

	if let Some(extension) = full_path.extension() {
		let extension = extension.to_string_lossy();
		let content_type = if TEXT_OUTPUT_EXTENSIONS
			.iter()
			.any(|&ext| ext == extension)
		{
			format!("text/{}", extension)
		} else if IMAGE_OUTPUT_EXTENSIONS.iter().any(|&ext| ext == extension) {
			format!("image/{}", extension)
		} else {
			let message =
				format!("Unrecognized extension: {}", full_path.display());
			println!("Responding with HTTP 500 error: {}", message);
			write_to_stream_log_count(
				format!("HTTP/1.1 500 Error\r\nContent-Type: text/html; charset=UTF-8\r\n\r\n<html><body>{}</body></html>\r\n", message).as_bytes(),
				&mut stream,
			);
			return;
		};
		write_to_stream_log_count(
			format!(
				"HTTP/1.1 200 OK\r\nContent-Type: {}; charset=UTF-8\r\n\r\n",
				content_type
			)
			.as_bytes(),
			&mut stream,
		);
		let mut buf = [0_u8; 64 * 1024];
		loop {
			let size = input_file.read(&mut buf).unwrap_or_else(|e| {
				panic!("Failed reading from {}: {}", full_path.display(), e);
			});
			if size < 1 {
				break;
			}

			write_to_stream_log_count(&buf[0..size], &mut stream);
		}
	} else {
		let message = format!("Missing extension: {}", full_path.display());
		println!("Responding with HTTP 500 error: {}", message);
		write_to_stream_log_count(
			format!("HTTP/1.1 500 Error\r\nContent-Type: text/html; charset=UTF-8\r\n\r\n<html><body>{}</body></html>\r\n", message).as_bytes(),
			&mut stream,
		)
	}
}
