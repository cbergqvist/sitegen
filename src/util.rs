use std::fs;
use std::io::Write;
use std::path::PathBuf;

pub const CSS_EXTENSION: &str = "css";
pub const HTML_EXTENSION: &str = "html";
pub const MARKDOWN_EXTENSION: &str = "md";
pub const XML_EXTENSION: &str = "xml";

pub struct Refresh {
	pub index: u32,
	pub file: Option<PathBuf>,
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
		let target = output_dir.join(
			file_name.strip_prefix(&input_prefix).unwrap_or_else(|e| {
				panic!(
					"Failed stripping {}-prefix from {}: {}",
					input_prefix.display(),
					file_name.display(),
					e
				)
			}),
		);
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