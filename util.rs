use std::io::Write;
use std::net::TcpStream;
use std::path::PathBuf;

pub struct Refresh {
	pub index: u32,
	pub file: Option<PathBuf>,
}

pub fn write(bytes: &[u8], stream: &mut TcpStream) {
	match stream.write_all(bytes) {
		Ok(()) => println!("Wrote {} bytes.", bytes.len()),
		Err(e) => println!("WARNING: Failed sending response: {}", e),
	}
}
