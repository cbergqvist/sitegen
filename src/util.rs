use std::fs;
use std::io::Write;
use std::path::PathBuf;

pub const CSS_EXTENSION: &str = "css";
pub const HTML_EXTENSION: &str = "html";
pub const MARKDOWN_EXTENSION: &str = "md";
pub const XML_EXTENSION: &str = "xml";

// Special identifier making JavaScript reload the current page.
pub const RELOAD_CURRENT: &str = "*";

pub struct Refresh {
	pub index: u32,
	pub file: Option<String>,
}

pub fn write_to_stream<T: Write>(buffer: &[u8], stream: &mut T) {
	stream.write_all(buffer).unwrap_or_else(|e| {
		panic!("Failed writing \"{:?}\" to to buffer: {}.", buffer, e)
	});
}

pub fn write_to_stream_log_count<T: Write>(buffer: &[u8], stream: &mut T) {
	write_to_stream(buffer, stream);
	println!("Wrote {} bytes.", buffer.len());
}

pub fn copy_files_with_prefix(
	files: &[PathBuf],
	input_dir: &PathBuf,
	output_dir: &PathBuf,
) {
	let mut input_prefix = input_dir.clone();
	if let Some(first) = files.first() {
		if first.is_absolute() {
			input_prefix = input_dir.canonicalize().unwrap_or_else(|e| {
				panic!("Failed to canonicalize {}: {}", input_dir.display(), e)
			});
		}
	}
	for file_name in files {
		let target = output_dir.join(strip_prefix(file_name, &input_prefix));
		fs::copy(file_name, &target).unwrap_or_else(|e| {
			panic!(
				"Failed copying {} to {}: {}",
				file_name.display(),
				target.display(),
				e
			)
		});
	}
}

pub fn strip_prefix(path: &PathBuf, prefix: &PathBuf) -> PathBuf {
	path.strip_prefix(prefix)
		.unwrap_or_else(|e| {
			panic!(
				"Failed stripping prefix \"{}\" from \"{}\": {}",
				prefix.display(),
				path.display(),
				e
			)
		})
		.to_path_buf()
}
