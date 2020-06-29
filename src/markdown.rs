use std::borrow::Borrow;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::Instant;

use crate::front_matter::FrontMatter;
use crate::util;
use crate::util::write_to_stream;

use pulldown_cmark::{html, Parser};

#[derive(Clone)]
pub struct OutputFile {
	pub path: PathBuf,
	pub group: Option<String>,
	pub front_matter: Option<FrontMatter>,
}

pub struct ComputedTemplatePath {
	pub path: PathBuf,
	pub group: Option<String>,
}

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
	input_output_map: &mut HashMap<PathBuf, OutputFile>,
) -> GeneratedFile {
	let timer = Instant::now();

	// TODO: Inserting different format?
	let output_file = input_output_map
		.entry(input_file_path.clone())
		.or_insert_with(|| {
			compute_output_path(
				input_file_path,
				root_input_dir,
				root_output_dir,
			)
		})
		.clone();

	let mut input_file =
		BufReader::new(fs::File::open(&input_file_path).unwrap_or_else(|e| {
			panic!("Failed opening \"{}\": {}.", &input_file_path.display(), e)
		}));

	let front_matter = if let Some(fm) = output_file.front_matter {
		fm
	} else {
		panic!(
			"Expecting at least a default FrontMatter instance on file: {}",
			output_file.path.display()
		)
	};
	input_file
		.seek(SeekFrom::Start(front_matter.end_position))
		.unwrap_or_else(|e| {
			panic!("Failed seeking in {}: {}", input_file_path.display(), e)
		});

	let output_file_path = output_file.path;

	let mut processed_markdown_content = BufWriter::new(Vec::new());

	process_liquid_content(
		&output_file_path,
		&mut processed_markdown_content,
		&front_matter,
		None,
		input_file_path,
		&mut input_file,
		root_input_dir,
		root_output_dir,
		input_output_map,
	);

	let markdown_content = String::from_utf8_lossy(
		&processed_markdown_content
			.into_inner()
			.unwrap_or_else(|e| panic!("into_inner() failed: {}", e)),
	)
	.to_string();

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

	process_liquid_content(
		&output_file_path,
		&mut output_buf,
		&front_matter,
		Some(&html_content),
		&template_path_result.path,
		&mut template_file,
		root_input_dir,
		root_output_dir,
		input_output_map,
	);

	write_buffer_to_file(output_buf.buffer(), &output_file_path);

	println!(
		"Converted {} to {} (using template {}) in {} ms.",
		input_file_path.display(),
		&output_file_path.display(),
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
	input_output_map: &mut HashMap<PathBuf, OutputFile>,
) -> PathBuf {
	let timer = Instant::now();

	// TODO: Inserting different format?
	let output_file = input_output_map
		.entry(input_file_path.clone())
		.or_insert_with(|| {
			compute_output_path(
				input_file_path,
				root_input_dir,
				root_output_dir,
			)
		})
		.clone();

	let mut input_file =
		BufReader::new(fs::File::open(&input_file_path).unwrap_or_else(|e| {
			panic!("Failed opening \"{}\": {}.", &input_file_path.display(), e)
		}));

	let front_matter = if let Some(fm) = output_file.front_matter {
		fm
	} else {
		panic!(
			"Expecting at least a default FrontMatter instance on file: {}",
			output_file.path.display()
		)
	};
	input_file
		.seek(SeekFrom::Start(front_matter.end_position))
		.unwrap_or_else(|e| {
			panic!("Failed seeking in {}: {}", input_file_path.display(), e)
		});

	let output_file_path = output_file.path;

	let mut output_buf = BufWriter::new(Vec::new());

	process_liquid_content(
		&output_file_path,
		&mut output_buf,
		&front_matter,
		None,
		input_file_path,
		&mut input_file,
		root_input_dir,
		root_output_dir,
		input_output_map,
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

pub fn compute_output_path(
	input_file_path: &PathBuf,
	root_input_dir: &PathBuf,
	root_output_dir: &PathBuf,
) -> OutputFile {
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

	let mut input_file =
		BufReader::new(fs::File::open(&input_file_path).unwrap_or_else(|e| {
			panic!("Failed opening \"{}\": {}.", &input_file_path.display(), e)
		}));

	let front_matter =
		crate::front_matter::parse(input_file_path, &mut input_file);

	let mut group = None;
	let input_file_parent = input_file_path
		.parent()
		.unwrap_or_else(|| {
			panic!("Failed to get parent from: {}", input_file_path.display())
		})
		.file_stem()
		.unwrap_or_else(|| {
			panic!(
				"Expected file stem on parent of: {}",
				input_file_path.display()
			)
		})
		.to_string_lossy();
	if input_file_parent.ends_with('s') {
		group = Some(input_file_parent.to_string());
	}

	OutputFile {
		path,
		group,
		front_matter: Some(front_matter),
	}
}

// Rolling a simple version of Liquid parsing on my own since the official Rust
// one has too many dependencies.
//
// Allowing more lines to keep state machine cohesive.
#[allow(clippy::too_many_lines)]
pub fn process_liquid_content<T: Read>(
	output_file_path: &PathBuf,
	mut output_buf: &mut BufWriter<Vec<u8>>,
	front_matter: &FrontMatter,
	html_content: Option<&str>,
	input_file_path: &PathBuf,
	input_file: &mut BufReader<T>,
	root_input_dir: &PathBuf,
	root_output_dir: &PathBuf,
	input_output_map: &HashMap<PathBuf, OutputFile>,
) {
	#[derive(Debug)]
	enum State {
		RegularContent,
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

	let mut state = State::RegularContent;
	let mut buf = [0_u8; 64 * 1024];
	let mut line_number = 1;
	let mut column_number = 1;
	let mut object = Vec::new();
	let mut field = Vec::new();
	let mut function = Vec::new();
	let mut parameter = Vec::new();
	loop {
		let size = input_file.read(&mut buf).unwrap_or_else(|e| {
			panic!(
				"Failed reading from template file {}: {}",
				input_file_path.display(),
				e
			)
		});
		if size == 0 {
			break;
		}

		for &byte in &buf[0..size] {
			if byte == b'\n' {
				match state {
					State::RegularContent => { write_to_stream(&[byte], output_buf); }
					State::LastOpenBracket => {
						write_to_stream(b"{\n", output_buf);
						state = State::RegularContent
					}
					State::ValueObject => panic!("Unexpected newline while reading value object identifier at {}:{}:{}.", input_file_path.display(), line_number, column_number),
					State::ValueField => {
						output_template_value(&mut output_buf, &mut object, &mut field, front_matter, html_content);
						state = State::ValueEnd
					}
					State::WaitingForCloseBracket => panic!("Expected close bracket but got newline at {}:{}:{}.", input_file_path.display(), line_number, column_number),
					State::TagFunction => panic!("Unexpected newline in the middle of function at {}:{}:{}.", input_file_path.display(), line_number, column_number),
					State::TagParameter => {
						run_function(output_file_path, &mut output_buf, &mut function, &mut parameter, front_matter, html_content, input_file_path, root_input_dir, root_output_dir, input_output_map);
						state = State::TagEnd
					}
					State::ValueStart | State::ValueEnd | State::TagStart | State::TagEnd => {}
				}
				line_number += 1;
				column_number = 1;
			} else {
				match state {
					State::RegularContent => {
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
								state = State::RegularContent;
							}
						}
					}
					State::ValueStart => match byte {
						b'{' => panic!("Unexpected open bracket while in template mode at {}:{}:{}.", input_file_path.display(), line_number, column_number),
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
							b'.' => panic!("Additional dot in template identifier at {}:{}:{}.", input_file_path.display(), line_number, column_number),
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
							_ => panic!("Unexpected non-whitespace character at {}:{}:{}.", input_file_path.display(), line_number, column_number)
						}
					}
					State::TagStart => {
						match byte {
							b'%' => panic!("Unexpected % following tag start at {}:{}:{}.", input_file_path.display(), line_number, column_number),
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
									run_function(output_file_path, &mut output_buf, &mut function, &mut parameter, front_matter, html_content, input_file_path, root_input_dir, root_output_dir, input_output_map);
									state = State::TagEnd;
								}
							}
							b'%' => {
								panic!("Unexpected end of parameter at {}:{}:{}.", input_file_path.display(), line_number, column_number)
							}
							_ => parameter.push(byte)

						}
					}
					State::TagEnd => {
						match byte {
							b'%' => state = State::WaitingForCloseBracket,
							b' ' | b'\t' => {}
							_ => panic!("Unexpected non-whitespace character at {}:{}:{}.", input_file_path.display(), line_number, column_number)
						}
					}
					State::WaitingForCloseBracket => {
						if byte == b'}' {
							state = State::RegularContent;
						} else {
							panic!("Missing double close-bracket at {}:{}:{}.", input_file_path.display(), line_number, column_number)
						}
					}
				}
				column_number += 1
			}
		}
	}

	match state {
		State::RegularContent => {}
		_ => panic!(
			"Content of {} ended while still in state: {:?}",
			input_file_path.display(),
			state
		),
	}
}

fn compute_template_path(
	input_file_path: &PathBuf,
	root_input_dir: &PathBuf,
) -> ComputedTemplatePath {
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

	ComputedTemplatePath {
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
			if let Some(date) = &front_matter.date {
				write_to_stream(date.as_bytes(), &mut output_buf)
			} else {
				panic!("Requested date but none exists {}", front_matter.title)
			}
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
	output_file_path: &PathBuf,
	mut output_buf: &mut BufWriter<Vec<u8>>,
	function: &mut Vec<u8>,
	parameter: &mut Vec<u8>,
	front_matter: &FrontMatter,
	html_content: Option<&str>,
	template_file_path: &PathBuf,
	root_input_dir: &PathBuf,
	root_output_dir: &PathBuf,
	input_output_map: &HashMap<PathBuf, OutputFile>,
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
		"include" => include_file(
			output_file_path,
			&mut output_buf,
			&parameter_str,
			front_matter,
			html_content,
			template_file_path,
			root_input_dir,
			root_output_dir,
			input_output_map,
		),
		"link" => check_and_emit_link(
			output_file_path,
			&mut output_buf,
			&parameter_str,
			root_input_dir,
			root_output_dir,
			input_output_map,
		),
		_ => panic!("Unsupported function: {}", function_str),
	}
	function.clear();
	parameter.clear();
}

fn include_file(
	output_file_path: &PathBuf,
	mut output_buf: &mut BufWriter<Vec<u8>>,
	parameter_str: &str,
	front_matter: &FrontMatter,
	html_content: Option<&str>,
	template_file_path: &PathBuf,
	root_input_dir: &PathBuf,
	root_output_dir: &PathBuf,
	input_output_map: &HashMap<PathBuf, OutputFile>,
) {
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

	process_liquid_content(
		output_file_path,
		&mut output_buf,
		front_matter,
		html_content,
		&included_file_path,
		&mut included_file,
		root_input_dir,
		root_output_dir,
		input_output_map,
	)
}

fn check_and_emit_link(
	output_file_path: &PathBuf,
	output_buf: &mut BufWriter<Vec<u8>>,
	parameter_str: &str,
	root_input_dir: &PathBuf,
	root_output_dir: &PathBuf,
	input_output_map: &HashMap<PathBuf, OutputFile>,
) {
	let append_index_html = parameter_str.ends_with('/');
	if !parameter_str.starts_with('/') {
		panic!(
			"Only absolute paths are allowed in links, but got: {}",
			parameter_str
		);
	}
	let mut path = root_input_dir.join(PathBuf::from(&parameter_str[1..]));
	if append_index_html {
		path = path.join(PathBuf::from("index.html"));
	}
	if let Some(linked_output) = input_output_map.get(&path) {
		let mut equal_prefix = PathBuf::new();
		let mut equal_component_count = 0;
		for (self_component, link_component) in output_file_path
			.components()
			.zip(linked_output.path.components())
		{
			if self_component != link_component {
				break;
			}
			equal_prefix = equal_prefix.join(self_component);
			equal_component_count += 1;
		}
		if equal_prefix.iter().next() == None {
			panic!("No common prefix, expected at least {} but own path is {} and link is {}.", root_output_dir.display(), output_file_path.display(), linked_output.path.display());
		}

		assert!(
			output_file_path.starts_with(root_output_dir),
			"Expected {} to start with {}.",
			output_file_path.display(),
			root_output_dir.display()
		);

		// Do not strip own file name from link if path is the same.
		if output_file_path == &linked_output.path {
			equal_prefix.pop();
		}

		let own_component_count = output_file_path.components().count();
		let linked_component_count = linked_output.path.components().count();
		let mut base = PathBuf::new();
		if own_component_count > linked_component_count {
			for _i in 0..(own_component_count - linked_component_count) {
				base = base.join("../");
			}
		} else if own_component_count > equal_component_count + 1 {
			for _i in 0..((own_component_count - 1) - equal_component_count) {
				base = base.join("../");
			}
		} else {
			base = PathBuf::from("./");
		}

		let mut prefix_plus_slash = equal_prefix.to_string_lossy().to_string();
		prefix_plus_slash.push('/');
		let mut linked_output_path_stripped = base.join(
			linked_output
				.path
				.strip_prefix(&prefix_plus_slash)
				.unwrap_or_else(|e| {
					panic!(
						"Failed stripping prefix {} from {}: {}",
						prefix_plus_slash,
						linked_output.path.display(),
						e
					)
				}),
		);

		let mut append_trailing_slash = false;
		if linked_output_path_stripped.file_name()
			== Some(OsStr::new("index.html"))
		{
			linked_output_path_stripped.pop();
			append_trailing_slash = true;
		}

		let mut linked_output_path_stripped_str =
			linked_output_path_stripped.to_string_lossy().to_string();
		if append_trailing_slash {
			linked_output_path_stripped_str.push('/');
		}

		println!("File: {}, original link: {}, translated: {}, prefix+slash: {}, result: {}", output_file_path.display(), parameter_str, linked_output.path.display(), prefix_plus_slash, &linked_output_path_stripped_str);

		write_to_stream(linked_output_path_stripped_str.as_bytes(), output_buf);
	} else {
		panic!(
			"Failed finding {} among: {:#?}",
			path.display(),
			input_output_map.keys()
		);
	}
}
