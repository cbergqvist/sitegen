use std::borrow::Borrow;
use std::ffi::OsStr;
use std::fs;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::time::Instant;

use crate::front_matter::FrontMatter;
use crate::util;
use crate::util::write_to_stream;

use pulldown_cmark::{html, Parser};

pub struct GeneratedFile {
	pub path: PathBuf,
	pub group: Option<String>,
	pub front_matter: FrontMatter,
	pub html_content: String,
}

pub struct InputFileCollection {
	pub html: Vec<PathBuf>,
	pub markdown: Vec<PathBuf>,
	pub raw: Vec<PathBuf>,
}

impl InputFileCollection {
	pub const fn new() -> Self {
		Self {
			html: Vec::new(),
			markdown: Vec::new(),
			raw: Vec::new(),
		}
	}

	pub fn is_empty(&self) -> bool {
		self.html.is_empty() || self.markdown.is_empty() || self.raw.is_empty()
	}

	fn append(&mut self, other: &mut Self) {
		self.html.append(&mut other.html);
		self.markdown.append(&mut other.markdown);
		self.raw.append(&mut other.raw);
	}
}

struct ComputedFilePath {
	path: PathBuf,
	group: Option<String>,
}

pub fn get_files(input_dir: &PathBuf) -> InputFileCollection {
	let css_extension = OsStr::new(util::CSS_EXTENSION);
	let html_extension = OsStr::new(util::HTML_EXTENSION);
	let markdown_extension = OsStr::new(util::MARKDOWN_EXTENSION);

	let entries = fs::read_dir(input_dir).unwrap_or_else(|e| {
		panic!(
			"Failed reading paths from \"{}\": {}.",
			input_dir.display(),
			e
		)
	});
	let mut result = InputFileCollection::new();
	for entry in entries {
		match entry {
			Ok(entry) => {
				let path = entry.path();
				if let Ok(ft) = entry.file_type() {
					if ft.is_file() {
						if let Some(extension) = path.extension() {
							let recognized = || {
								println!(
									"File with recognized extension: \"{}\"",
									entry.path().display()
								)
							};
							if extension == html_extension {
								result.html.push(path);
								recognized();
							} else if extension == markdown_extension {
								result.markdown.push(path);
								recognized();
							} else if extension == css_extension {
								result.raw.push(path);
								recognized();
							} else {
								println!(
									"Skipping file with unrecognized extension ({}) file: \"{}\"",
									extension.to_string_lossy(),
									entry.path().display()
								);
							}
						} else {
							println!(
								"Skipping extension-less file: \"{}\"",
								entry.path().display()
							);
						}
					} else if ft.is_dir() {
						let file_name = path.file_name().unwrap_or_else(|| {
							panic!(
								"Directory without filename?: {}",
								path.display()
							)
						});
						if file_name.to_string_lossy().starts_with('_') {
							println!(
								"Skipping '_'-prefixed dir: {}",
								path.display()
							);
						} else if file_name.to_string_lossy().starts_with('.') {
							println!(
								"Skipping '.'-prefixed dir: {}",
								path.display()
							);
						} else {
							let mut subdir_files = self::get_files(&path);
							result.append(&mut subdir_files);
						}
					} else {
						println!("Skipping non-file/dir {}", path.display());
					}
				} else {
					println!(
						"WARNING: Failed getting file type of {}.",
						entry.path().display()
					);
				}
			}
			Err(e) => println!(
				"WARNING: Invalid entry in \"{}\": {}",
				input_dir.display(),
				e
			),
		}
	}

	result
}

pub fn process_file(
	input_file_path: &PathBuf,
	root_input_dir: &PathBuf,
	root_output_dir: &PathBuf,
) -> GeneratedFile {
	let timer = Instant::now();
	let mut input_file =
		BufReader::new(fs::File::open(&input_file_path).unwrap_or_else(|e| {
			panic!("Failed opening \"{}\": {}.", &input_file_path.display(), e)
		}));

	let front_matter =
		crate::front_matter::parse(input_file_path, &mut input_file);

	let mut markdown_content = String::new();
	let _size = input_file
		.read_to_string(&mut markdown_content)
		.unwrap_or_else(|e| {
			panic!(
				"Failed reading Markdown content from \"{}\": {}.",
				&input_file_path.display(),
				e
			)
		});

	let mut output_buf = BufWriter::new(Vec::new());
	let template_path_result =
		compute_template_path(input_file_path, root_input_dir);

	let mut html_content = String::new();
	html::push_html(&mut html_content, Parser::new(&markdown_content));

	let mut template_file = BufReader::new(
		fs::File::open(&template_path_result.path).unwrap_or_else(|e| {
			panic!(
				"Failed opening template file {}: {}",
				template_path_result.path.display(),
				e
			)
		}),
	);

	write_html_page(
		&mut output_buf,
		&front_matter,
		Some(&html_content),
		&template_path_result.path,
		&mut template_file,
		root_input_dir,
	);

	let output_file_path =
		compute_output_path(input_file_path, root_input_dir, root_output_dir);

	write_buffer_to_file(output_buf.buffer(), &output_file_path);

	println!(
		"Converted {} to {} (using template {}) in {} ms.",
		input_file_path.display(),
		output_file_path.display(),
		template_path_result.path.display(),
		timer.elapsed().as_millis()
	);

	GeneratedFile {
		path: output_file_path
			.strip_prefix(root_output_dir)
			.unwrap_or_else(|e| {
				panic!(
					"Failed stripping prefix \"{}\" from \"{}\": {}",
					root_output_dir.display(),
					output_file_path.display(),
					e
				)
			})
			.to_path_buf(),
		group: template_path_result.group,
		front_matter,
		html_content,
	}
}

pub fn process_template_file_without_markdown(
	input_file_path: &PathBuf,
	root_input_dir: &PathBuf,
	root_output_dir: &PathBuf,
) -> PathBuf {
	let timer = Instant::now();
	let mut input_file =
		BufReader::new(fs::File::open(&input_file_path).unwrap_or_else(|e| {
			panic!("Failed opening \"{}\": {}.", &input_file_path.display(), e)
		}));

	let front_matter =
		crate::front_matter::parse(input_file_path, &mut input_file);

	let output_file_path =
		compute_output_path(input_file_path, root_input_dir, root_output_dir);

	let mut output_buf = BufWriter::new(Vec::new());

	write_html_page(
		&mut output_buf,
		&front_matter,
		None,
		input_file_path,
		&mut input_file,
		root_input_dir,
	);

	write_buffer_to_file(output_buf.buffer(), &output_file_path);

	println!(
		"Processed markdown-less {} to {} in {} ms.",
		input_file_path.display(),
		output_file_path.display(),
		timer.elapsed().as_millis(),
	);

	output_file_path
		.strip_prefix(root_output_dir)
		.unwrap_or_else(|e| {
			panic!(
				"Failed stripping prefix \"{}\" from \"{}\": {}",
				root_output_dir.display(),
				output_file_path.display(),
				e
			)
		})
		.to_path_buf()
}

fn write_buffer_to_file(buffer: &[u8], path: &PathBuf) {
	let closest_output_dir = path.parent().unwrap_or_else(|| {
		panic!(
			"Output file path without a parent directory?: {}",
			path.display()
		)
	});
	fs::create_dir_all(closest_output_dir).unwrap_or_else(|e| {
		panic!(
			"Failed creating directories for {}: {}",
			closest_output_dir.display(),
			e
		)
	});

	let mut output_file = fs::File::create(&path).unwrap_or_else(|e| {
		panic!("Failed creating \"{}\": {}.", &path.display(), e)
	});
	output_file.write_all(buffer).unwrap_or_else(|e| {
		panic!("Failed writing to \"{}\": {}.", &path.display(), e)
	});

	// Avoiding sync_all() for now to be friendlier to disks.
	output_file.sync_data().unwrap_or_else(|e| {
		panic!("Failed sync_data() for \"{}\": {}.", &path.display(), e)
	});
}

fn compute_output_path(
	input_file_path: &PathBuf,
	root_input_dir: &PathBuf,
	root_output_dir: &PathBuf,
) -> PathBuf {
	let mut path = root_output_dir.clone();
	if input_file_path.starts_with(root_input_dir) {
		path.push(
			input_file_path
				.strip_prefix(root_input_dir)
				.unwrap_or_else(|e| {
					panic!(
						"Failed stripping prefix \"{}\" from \"{}\": {}",
						root_input_dir.display(),
						input_file_path.display(),
						e
					)
				})
				.with_extension("html"),
		);
	} else {
		let full_root_input_path = fs::canonicalize(root_input_dir)
			.unwrap_or_else(|e| {
				panic!(
					"Failed to canonicalize {}: {}",
					root_input_dir.display(),
					e
				)
			});
		if input_file_path.starts_with(&full_root_input_path) {
			path.push(
				&input_file_path
					.strip_prefix(&full_root_input_path)
					.unwrap_or_else(|e| {
						panic!(
							"Failed stripping prefix \"{}\" from \"{}\": {}",
							full_root_input_path.display(),
							input_file_path.display(),
							e
						)
					})
					.with_extension("html"),
			);
		} else {
			panic!(
				"Unable to handle input file name: {}",
				input_file_path.display()
			)
		}
	}

	path
}

// Rolling a simple version of Liquid parsing on my own since the official Rust
// one has too many dependencies.
//
// Allowing more lines to keep state machine cohesive.
#[allow(clippy::too_many_lines)]
fn write_html_page(
	mut output_buf: &mut BufWriter<Vec<u8>>,
	front_matter: &FrontMatter,
	html_content: Option<&str>,
	template_file_path: &PathBuf,
	template_file: &mut BufReader<fs::File>,
	root_input_dir: &PathBuf,
) {
	enum State {
		JustHtml,
		LastOpenBracket,
		ValueStart,
		ValueObject,
		ValueField,
		ValueEnd,
		TagStart,
		TagFunction,
		TagParameter,
		TagEnd,
		WaitingForCloseBracket,
	}

	let mut state = State::JustHtml;
	let mut buf = [0_u8; 64 * 1024];
	let mut line_number = 1;
	let mut column_number = 1;
	let mut object = Vec::new();
	let mut field = Vec::new();
	let mut function = Vec::new();
	let mut parameter = Vec::new();
	loop {
		let size = template_file.read(&mut buf).unwrap_or_else(|e| {
			panic!(
				"Failed reading from template file {}: {}",
				template_file_path.display(),
				e
			)
		});
		if size == 0 {
			break;
		}

		for &byte in &buf[0..size] {
			if byte == b'\n' {
				match state {
					State::JustHtml => { write_to_stream(&[byte], output_buf); }
					State::LastOpenBracket => {
						write_to_stream(b"{\n", output_buf);
						state = State::JustHtml
					}
					State::ValueObject => panic!("Unexpected newline while reading value object identifier at {}:{}:{}.", template_file_path.display(), line_number, column_number),
					State::ValueField => {
						output_template_value(&mut output_buf, &mut object, &mut field, front_matter, html_content);
						state = State::ValueEnd
					}
					State::WaitingForCloseBracket => panic!("Expected close bracket but got newline at {}:{}:{}.", template_file_path.display(), line_number, column_number),
					State::TagFunction => panic!("Unexpected newline in the middle of function at {}:{}:{}.", template_file_path.display(), line_number, column_number),
					State::TagParameter => {
						run_function(&mut output_buf, &mut function, &mut parameter, front_matter, html_content, template_file_path, root_input_dir);
						state = State::TagEnd
					}
					State::ValueStart | State::ValueEnd | State::TagStart | State::TagEnd => {}
				}
				line_number += 1;
				column_number = 1;
			} else {
				match state {
					State::JustHtml => {
						match byte {
							b'{' => state = State::LastOpenBracket,
							_ => write_to_stream(&[byte], output_buf)
						}
					}
					State::LastOpenBracket => {
						match byte {
							b'{' =>	state = State::ValueStart,
							b'%' => state = State::TagStart,
							_ => {
								write_to_stream(&[b'{'], output_buf);
								state = State::JustHtml;
							}
						}
					}
					State::ValueStart => match byte {
						b'{' => panic!("Unexpected open bracket while in template mode at {}:{}:{}.", template_file_path.display(), line_number, column_number),
						b' ' | b'\t' => {},
						_ => {
							object.push(byte);
							state = State::ValueObject;
						}
					}
					State::ValueObject => {
						if byte == b'.' {
							state = State::ValueField;
						} else {
							object.push(byte);
						}
					}
					State::ValueField => {
						match byte {
							b'.' => panic!("Additional dot in template identifier at {}:{}:{}.", template_file_path.display(), line_number, column_number),
							b'}' => state = State::WaitingForCloseBracket,
							b' ' | b'\t' => {
								output_template_value(&mut output_buf, &mut object, &mut field, front_matter, html_content);
								state = State::ValueEnd
							}
							_ => field.push(byte)
						}
					}
					State::ValueEnd => {
						match byte {
							b'}' => state = State::WaitingForCloseBracket,
							b' ' | b'\t' => {}
							_ => panic!("Unexpected non-whitespace character at {}:{}:{}.", template_file_path.display(), line_number, column_number)
						}
					}
					State::TagStart => {
						match byte {
							b'%' => panic!("Unexpected % following tag start at {}:{}:{}.", template_file_path.display(), line_number, column_number),
							b' ' | b'\t' => {}
							_ => {
								function.push(byte);
								state = State::TagFunction;
							}
						}
					}
					State::TagFunction => {
						match byte {
							b' ' | b'\t' => state = State::TagParameter,
							_ => function.push(byte)

						}
					}
					State::TagParameter => {
						match byte {
							b' ' | b'\t' => {
								if !parameter.is_empty() {
									run_function(&mut output_buf, &mut function, &mut parameter, front_matter, html_content, template_file_path, root_input_dir);
									state = State::TagEnd;
								}
							}
							_ => parameter.push(byte)

						}
					}
					State::TagEnd => {
						match byte {
							b'%' => state = State::WaitingForCloseBracket,
							b' ' | b'\t' => {}
							_ => panic!("Unexpected non-whitespace character at {}:{}:{}.", template_file_path.display(), line_number, column_number)
						}
					}
					State::WaitingForCloseBracket => {
						if byte == b'}' {
							state = State::JustHtml;
						} else {
							panic!("Missing double close-bracket at {}:{}:{}.", template_file_path.display(), line_number, column_number)
						}
					}
				}
				column_number += 1
			}
		}
	}
}

fn compute_template_path(
	input_file_path: &PathBuf,
	root_input_dir: &PathBuf,
) -> ComputedFilePath {
	let mut template_file_path = root_input_dir.join(PathBuf::from("_layouts"));
	let input_file_parent = input_file_path.parent().unwrap_or_else(|| {
		panic!("Failed to get parent from: {}", input_file_path.display())
	});
	let mut root_input_dir_corrected = root_input_dir.clone();
	if !input_file_parent.starts_with(root_input_dir) {
		let full_root_input_path = fs::canonicalize(root_input_dir)
			.unwrap_or_else(|e| {
				panic!(
					"Failed to canonicalize {}: {}",
					root_input_dir.display(),
					e
				)
			});
		if input_file_path.starts_with(&full_root_input_path) {
			root_input_dir_corrected = full_root_input_path
		}
	}
	let mut template_name = if input_file_parent == root_input_dir_corrected {
		input_file_path
			.file_name()
			.unwrap_or_else(|| {
				panic!(
					"Missing file name in path: {}",
					input_file_path.display()
				)
			})
			.to_string_lossy()
			.to_string()
	} else {
		input_file_parent
			.file_name()
			.unwrap_or_else(|| {
				panic!(
					"Failed to get file name of parent of: {}",
					input_file_path.display()
				)
			})
			.to_string_lossy()
			.to_string()
	};
	let mut group = None;
	if template_name.ends_with('s') {
		group = Some(template_name.clone());
		template_name.truncate(template_name.len() - 1)
	}
	template_file_path.push(template_name);
	template_file_path.set_extension("html");
	if !template_file_path.exists() {
		let mut default_template = template_file_path.clone();
		default_template.set_file_name("default.html");
		if !default_template.exists() {
			panic!(
				"Failed resolving template file for: {}, tried with {} and {}",
				input_file_path.display(),
				template_file_path.display(),
				default_template.display(),
			);
		}
		template_file_path = default_template;
	}

	ComputedFilePath {
		path: template_file_path,
		group,
	}
}

fn output_template_value(
	mut output_buf: &mut BufWriter<Vec<u8>>,
	object: &mut Vec<u8>,
	field: &mut Vec<u8>,
	front_matter: &FrontMatter,
	html_content: Option<&str>,
) {
	if object.is_empty() {
		panic!("Empty object name.")
	}
	if field.is_empty() {
		panic!("Empty field name.")
	}

	let object_str = String::from_utf8_lossy(object);
	if object_str != "page" {
		panic!("Unhandled object \"{}\"", object_str);
	}

	let field_str = String::from_utf8_lossy(field);
	match field_str.borrow() {
		"content" => {
			if let Some(content) = html_content {
				write_to_stream(content.as_bytes(), &mut output_buf)
			} else {
				panic!("Requested content but none exists")
			}
		}
		"date" => {
			write_to_stream(front_matter.date.as_bytes(), &mut output_buf)
		}
		"title" => {
			write_to_stream(front_matter.title.as_bytes(), &mut output_buf)
		}
		"published" => write_to_stream(
			if front_matter.published {
				b"true"
			} else {
				b"false"
			},
			&mut output_buf,
		),
		"edited" => {
			if let Some(edited) = &front_matter.edited {
				write_to_stream(edited.as_bytes(), &mut output_buf)
			}
		}
		// TODO: categories
		// TODO: tags
		// TODO: layout
		_ => {
			if let Some(value) = front_matter.custom_attributes.get(&*field_str)
			{
				write_to_stream(value.as_bytes(), &mut output_buf)
			} else {
				panic!("Not yet supported field: {}.{}", object_str, field_str)
			}
		}
	}
	object.clear();
	field.clear();
}

fn run_function(
	mut output_buf: &mut BufWriter<Vec<u8>>,
	function: &mut Vec<u8>,
	parameter: &mut Vec<u8>,
	front_matter: &FrontMatter,
	html_content: Option<&str>,
	template_file_path: &PathBuf,
	root_input_dir: &PathBuf,
) {
	if function.is_empty() {
		panic!("Empty function name.")
	}
	if parameter.is_empty() {
		panic!("Empty parameter.")
	}

	let function_str = String::from_utf8_lossy(function);
	let parameter_str = String::from_utf8_lossy(parameter);
	match function_str.borrow() {
		"include" => {
			let included_file_path =
				root_input_dir.join("_includes").join(&*parameter_str);

			let mut included_file = BufReader::new(
				fs::File::open(&included_file_path).unwrap_or_else(|e| {
					panic!(
						"Failed opening \"{}\": {}.",
						&included_file_path.display(),
						e
					)
				}),
			);

			println!(
				"Including {} into {}.",
				included_file_path.display(),
				template_file_path.display()
			);

			write_html_page(
				&mut output_buf,
				front_matter,
				html_content,
				&included_file_path,
				&mut included_file,
				root_input_dir,
			)
		}
		_ => panic!("Unsupported function: {}", function_str),
	}
	function.clear();
	parameter.clear();
}
