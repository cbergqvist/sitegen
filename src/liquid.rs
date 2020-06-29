use std::borrow::Borrow;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io::{BufReader, BufWriter, Read};
use std::path::PathBuf;

use crate::front_matter::FrontMatter;
use crate::markdown::OutputFile;
use crate::util::write_to_stream;

pub struct Context<'a> {
	pub input_file_path: &'a PathBuf,
	pub output_file_path: &'a PathBuf,
	pub front_matter: &'a FrontMatter,
	pub html_content: Option<&'a str>,
	pub root_input_dir: &'a PathBuf,
	pub root_output_dir: &'a PathBuf,
}
// Rolling a simple version of Liquid parsing on my own since the official Rust
// one has too many dependencies.
//
// Allowing more lines to keep state machine cohesive.
#[allow(clippy::too_many_lines)]
pub fn process<T: Read>(
	input_file: &mut BufReader<T>,
	mut output_buf: &mut BufWriter<Vec<u8>>,
	input_output_map: &HashMap<PathBuf, OutputFile>,
	context: &Context,
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
	let panic_at_location = |m, l, c| -> ! {
		panic!(
			"{} Location: {}:{}:{}.",
			m,
			context.input_file_path.display(),
			l,
			c
		)
	};
	loop {
		let size = input_file.read(&mut buf).unwrap_or_else(|e| {
			panic!(
				"Failed reading from template file {}: {}",
				context.input_file_path.display(),
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
					State::ValueObject => panic_at_location("Unexpected newline while reading value object identifier.", line_number, column_number),
					State::ValueField => {
						output_template_value(&mut output_buf, &mut object, &mut field, context.front_matter, context.html_content);
						state = State::ValueEnd
					}
					State::WaitingForCloseBracket => panic_at_location("Expected close bracket but got newline.", line_number, column_number),
					State::TagFunction => panic_at_location("Unexpected newline in the middle of function.", line_number, column_number),
					State::TagParameter => {
						run_function(&mut output_buf, &mut function, &mut parameter, input_output_map, context);
						state = State::TagEnd
					}
					State::ValueStart | State::ValueEnd | State::TagStart | State::TagEnd => {}
				}
				line_number += 1;
				column_number = 1;
			} else {
				match state {
					State::RegularContent => match byte {
						b'{' => state = State::LastOpenBracket,
						_ => write_to_stream(&[byte], output_buf),
					},
					State::LastOpenBracket => match byte {
						b'{' => state = State::ValueStart,
						b'%' => state = State::TagStart,
						_ => {
							write_to_stream(&[b'{'], output_buf);
							state = State::RegularContent;
						}
					},
					State::ValueStart => match byte {
						b'{' => panic_at_location(
							"Unexpected open bracket while in template mode.",
							line_number,
							column_number,
						),
						b' ' | b'\t' => {}
						_ => {
							object.push(byte);
							state = State::ValueObject;
						}
					},
					State::ValueObject => {
						if byte == b'.' {
							state = State::ValueField;
						} else {
							object.push(byte);
						}
					}
					State::ValueField => match byte {
						b'.' => panic_at_location(
							"Additional dot in template identifier.",
							line_number,
							column_number,
						),
						b'}' => state = State::WaitingForCloseBracket,
						b' ' | b'\t' => {
							output_template_value(
								&mut output_buf,
								&mut object,
								&mut field,
								context.front_matter,
								context.html_content,
							);
							state = State::ValueEnd
						}
						_ => field.push(byte),
					},
					State::ValueEnd => match byte {
						b'}' => state = State::WaitingForCloseBracket,
						b' ' | b'\t' => {}
						_ => panic_at_location(
							"Unexpected non-whitespace character.",
							line_number,
							column_number,
						),
					},
					State::TagStart => match byte {
						b'%' => panic_at_location(
							"Unexpected % following tag start.",
							line_number,
							column_number,
						),
						b' ' | b'\t' => {}
						_ => {
							function.push(byte);
							state = State::TagFunction;
						}
					},
					State::TagFunction => match byte {
						b' ' | b'\t' => state = State::TagParameter,
						_ => function.push(byte),
					},
					State::TagParameter => match byte {
						b' ' | b'\t' => {
							if !parameter.is_empty() {
								run_function(
									&mut output_buf,
									&mut function,
									&mut parameter,
									input_output_map,
									context,
								);
								state = State::TagEnd;
							}
						}
						b'%' => panic_at_location(
							"Unexpected end of parameter.",
							line_number,
							column_number,
						),
						_ => parameter.push(byte),
					},
					State::TagEnd => match byte {
						b'%' => state = State::WaitingForCloseBracket,
						b' ' | b'\t' => {}
						_ => panic_at_location(
							"Unexpected non-whitespace character.",
							line_number,
							column_number,
						),
					},
					State::WaitingForCloseBracket => {
						if byte == b'}' {
							state = State::RegularContent;
						} else {
							panic_at_location(
								"Missing double close-bracket.",
								line_number,
								column_number,
							)
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
			context.input_file_path.display(),
			state
		),
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
	mut output_buf: &mut BufWriter<Vec<u8>>,
	function: &mut Vec<u8>,
	parameter: &mut Vec<u8>,
	input_output_map: &HashMap<PathBuf, OutputFile>,
	context: &Context,
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
			&mut output_buf,
			&parameter_str,
			input_output_map,
			context,
		),
		"link" => check_and_emit_link(
			context.output_file_path,
			&mut output_buf,
			&parameter_str,
			context.root_input_dir,
			context.root_output_dir,
			input_output_map,
		),
		_ => panic!("Unsupported function: {}", function_str),
	}
	function.clear();
	parameter.clear();
}

fn include_file(
	mut output_buf: &mut BufWriter<Vec<u8>>,
	parameter_str: &str,
	input_output_map: &HashMap<PathBuf, OutputFile>,
	context: &Context,
) {
	let included_file_path = context
		.root_input_dir
		.join("_includes")
		.join(&*parameter_str);

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
		context.input_file_path.display()
	);

	process(
		&mut included_file,
		&mut output_buf,
		input_output_map,
		&Context {
			input_file_path: &included_file_path,
			..*context
		},
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

	let linked_output = match input_output_map.get(&path) {
		Some(lo) => lo,
		_ => panic!(
			"Failed finding {} among: {:#?}",
			path.display(),
			input_output_map.keys()
		),
	};

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

	let append_trailing_slash = if linked_output_path_stripped.file_name()
		== Some(OsStr::new("index.html"))
	{
		linked_output_path_stripped.pop();
		true
	} else {
		false
	};

	let mut linked_output_path_stripped_str =
		linked_output_path_stripped.to_string_lossy().to_string();
	if append_trailing_slash {
		linked_output_path_stripped_str.push('/');
	}

	println!("File: {}, original link: {}, translated: {}, prefix+slash: {}, result: {}", output_file_path.display(), parameter_str, linked_output.path.display(), prefix_plus_slash, &linked_output_path_stripped_str);

	write_to_stream(linked_output_path_stripped_str.as_bytes(), output_buf);
}
