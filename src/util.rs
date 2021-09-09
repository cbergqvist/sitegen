use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use crate::front_matter;
use crate::markdown::GroupedOptionOutputFile;

pub const ASCII_EXTENSION: &str = "asc";
pub const CSS_EXTENSION: &str = "css";
pub const GIF_EXTENSION: &str = "gif";
pub const HTML_EXTENSION: &str = "html";
pub const JPEG_EXTENSION: &str = "jpeg";
pub const JPG_EXTENSION: &str = "jpg";
pub const MARKDOWN_EXTENSION: &str = "md";
pub const PNG_EXTENSION: &str = "png";
pub const TXT_EXTENSION: &str = "txt";
pub const XML_EXTENSION: &str = "xml";

// Special identifier making JavaScript reload the current page.
pub const RELOAD_CURRENT: &str = "*";

pub struct Refresh {
	pub index: u32,
	pub file: Option<String>,
}

// Decided not to put email in there because I was worried it would drift away
// from the public key file linked in my about.md.
// Decided not to put base URL in there because I want to encourage paths that
// don't depend on it in html/md files.
pub struct SiteInfo<'a> {
	pub title: &'a str,
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
		let target =
			translate_input_to_output(file_name, &input_prefix, output_dir);
		let target_parent = target.parent().unwrap_or_else(|| {
			panic!("Failed fetching parent from {}.", target.display())
		});
		if !target_parent.exists() {
			fs::create_dir_all(target_parent).unwrap_or_else(|e| {
				panic!(
					"Failed creating directories in {}: {}",
					target_parent.display(),
					e
				)
			});
		}
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

pub fn translate_input_to_output(
	path: &PathBuf,
	input_dir: &PathBuf,
	output_dir: &PathBuf,
) -> PathBuf {
	let mut without_prefix = strip_prefix(path, input_dir);
	if without_prefix.components().next()
		== Some(std::path::Component::Normal(std::ffi::OsStr::new(
			"_static",
		))) {
		without_prefix =
			PathBuf::from(&((*without_prefix.to_string_lossy())[1..]));
	}

	output_dir.join(without_prefix)
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

pub fn make_relative(
	input_file_path: &PathBuf,
	input_dir: &PathBuf,
) -> PathBuf {
	assert!(input_file_path.is_absolute());
	if input_dir.is_absolute() {
		panic!(
			"Don't currently handle absolute input dirs: {}",
			input_dir.display()
		);
	}

	let absolute_input_dir = input_dir.canonicalize().unwrap_or_else(|e| {
		panic!("Canonicalization of {} failed: {}", input_dir.display(), e)
	});

	input_dir.join(strip_prefix(input_file_path, &absolute_input_dir))
}

pub fn capitalize(input: &str) -> String {
	let mut output = String::with_capacity(input.len());
	let mut chars = input.chars();
	if let Some(first) = chars.next() {
		for c in first.to_uppercase() {
			output.push(c);
		}
		for c in chars {
			output.push(c);
		}
	}
	output
}

pub fn find_newest_file<'a, T>(
	input_output_map: &'a HashMap<PathBuf, T>,
	input_dir: &PathBuf,
) -> Option<&'a T> {
	let mut newest_file = None;
	let mut newest_time = UNIX_EPOCH;

	let supported_extensions =
		[OsStr::new(HTML_EXTENSION), OsStr::new(MARKDOWN_EXTENSION)];

	let excluded_folder = input_dir.join("tags");

	for mapping in input_output_map {
		let input_file = mapping.0;
		let extension = if let Some(e) = input_file.extension() {
			e
		} else {
			continue;
		};

		if !supported_extensions.iter().any(|e| e == &extension)
			|| input_file.starts_with(&excluded_folder)
		{
			continue;
		}

		let unique_path = strip_prefix(input_file, input_dir);
		if unique_path.starts_with("_") || unique_path.starts_with(".") {
			continue;
		}

		let metadata = fs::metadata(input_file).unwrap_or_else(|e| {
			panic!(
				"Failed fetching metadata for {}: {}",
				input_file.display(),
				e
			)
		});

		let modified = metadata.modified().unwrap_or_else(|e| {
			panic!(
				"Failed fetching modified time for {}: {}",
				input_file.display(),
				e
			)
		});

		if modified > newest_time {
			newest_time = modified;
			newest_file = Some(mapping);
		}
	}

	if let Some((file, _)) = &newest_file {
		println!("Newest file: {}", &file.display());
	}

	newest_file.map(|p| p.1)
}

pub fn get_front_matter_and_output_path<'a>(
	input_path: &PathBuf,
	input_output_map: &'a HashMap<PathBuf, GroupedOptionOutputFile>,
	deploy: bool,
) -> Option<(&'a Arc<front_matter::FrontMatter>, &'a PathBuf)> {
	let output_file = input_output_map.get(input_path).unwrap_or_else(|| {
		panic!(
			"Failed finding {} among {:?}",
			input_path.display(),
			input_output_map.keys()
		)
	});
	let output_file_path = &output_file.file.path;
	let front_matter =
		output_file.file.front_matter.as_ref().unwrap_or_else(|| {
			panic!(
				"Expecting at least a default FrontMatter instance on file: {}",
				output_file_path.display()
			)
		});

	if deploy && !front_matter.published {
		None
	} else {
		Some((front_matter, output_file_path))
	}
}
