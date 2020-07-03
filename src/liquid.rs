use core::cmp::min;
use std::borrow::Borrow;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom};
use std::path::PathBuf;

use crate::front_matter::FrontMatter;
use crate::markdown::{OptionOutputFile, OutputFile};
use crate::util::write_to_stream;

pub struct Context<'a> {
	pub input_file_path: &'a PathBuf,
	pub output_file_path: &'a PathBuf,
	pub front_matter: &'a FrontMatter,
	pub html_content: Option<&'a str>,
	pub root_input_dir: &'a PathBuf,
	pub root_output_dir: &'a PathBuf,
	pub input_output_map: &'a HashMap<PathBuf, OptionOutputFile>,
	pub groups: &'a HashMap<String, Vec<OutputFile>>,
}

#[derive(Clone)]
struct Position {
	line: usize,
	column: usize,
}

#[derive(Clone, Debug)]
struct List {
	values: Vec<Value>,
}

#[derive(Clone, Debug)]
struct Dictionary {
	map: HashMap<&'static str, Value>,
}

#[derive(Clone, Debug)]
enum Value {
	Scalar(String),
	List(List),
	Dictionary(Dictionary),
}

struct LoopInfo {
	values: Vec<Value>,
	variable: String,
	index: usize,
	end: usize,
	buffer_start_position: u64,
}

// Rolling a simple version of Liquid parsing on my own since the official Rust
// one has too many dependencies.
//
// Allowing more lines to keep state machine cohesive.
#[allow(clippy::too_many_lines)]
pub fn process<T: Read + Seek>(
	input_file: &mut BufReader<T>,
	mut output_buf: &mut BufWriter<Vec<u8>>,
	context: &Context,
) {
	#[derive(Debug)]
	enum State {
		RegularContent,
		LastOpenBracket,
		ValueNextIdentifier,
		ValueInIdentifier,
		ValueEnd,
		TagStart,
		TagFunction,
		TagNextParameter,
		TagInParameter,
		TagEnd,
		WaitingForCloseBracket,
	}

	enum Char {
		OpenCurly,
		CloseCurly,
		Percent,
		Newline,
		Whitespace,
		Other(u8),
	}

	let mut state = State::RegularContent;
	let mut position = Position { line: 1, column: 1 };
	let mut current_identifier: Vec<u8> = Vec::new();
	let mut queued_identifiers: Vec<String> = Vec::new();
	let mut loop_stack: Vec<LoopInfo> = Vec::new();

	loop {
		let byte: u8 = {
			let mut buf = [0_u8; 1];
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
			buf[0]
		};

		let c = match byte {
			b'{' => Char::OpenCurly,
			b'}' => Char::CloseCurly,
			b'%' => Char::Percent,
			b'\n' | b'\r' => Char::Newline,
			b' ' | b'\t' => Char::Whitespace,
			_ => Char::Other(byte),
		};

		let skipping = loop_stack
			.last()
			.map_or(false, |loop_info| loop_info.values.is_empty());

		match state {
			State::RegularContent => match c {
				Char::OpenCurly => state = State::LastOpenBracket,
				_ => {
					if !skipping {
						write_to_stream(&[byte], output_buf)
					}
				}
			},
			State::LastOpenBracket => match c {
				Char::OpenCurly => state = State::ValueNextIdentifier,
				Char::Percent => state = State::TagStart,
				Char::CloseCurly
				| Char::Newline
				| Char::Whitespace
				| Char::Other(..) => {
					if !skipping {
						write_to_stream(&[b'{', byte], output_buf)
					}
					state = State::RegularContent;
				}
			},
			State::ValueNextIdentifier => match c {
				Char::OpenCurly => panic_at_location(
					"Unexpected open bracket while in template mode.",
					&position,
					context,
				),
				Char::CloseCurly | Char::Newline => {
					assert!(current_identifier.is_empty());

					if !skipping {
						output_template_value(
							&mut output_buf,
							&queued_identifiers,
							&loop_stack,
							context,
						);
					}

					current_identifier.clear();
					queued_identifiers.clear();
					state = match c {
						Char::CloseCurly => State::WaitingForCloseBracket,
						Char::Newline => State::ValueEnd,
						_ => panic!("WTF?"),
					}
				}
				Char::Whitespace => {}
				Char::Percent | Char::Other(..) => {
					current_identifier.push(byte);
					state = State::ValueInIdentifier;
				}
			},
			State::ValueInIdentifier => match c {
				Char::CloseCurly => state = State::WaitingForCloseBracket,
				Char::Whitespace => {
					assert!(!current_identifier.is_empty());
					queued_identifiers.push(
						String::from_utf8_lossy(&current_identifier)
							.to_string(),
					);
					current_identifier.clear();
					state = State::ValueNextIdentifier
				}
				Char::Newline => {
					if !current_identifier.is_empty()
						|| !queued_identifiers.is_empty()
					{
						if !current_identifier.is_empty() {
							queued_identifiers.push(
								String::from_utf8_lossy(&current_identifier)
									.to_string(),
							);
						}
						if !skipping {
							output_template_value(
								&mut output_buf,
								&queued_identifiers,
								&loop_stack,
								context,
							)
						}
						current_identifier.clear();
						queued_identifiers.clear();
						state = State::ValueEnd
					}
				}
				Char::OpenCurly | Char::Percent | Char::Other(..) => {
					current_identifier.push(byte)
				}
			},
			State::ValueEnd => match c {
				Char::CloseCurly => state = State::WaitingForCloseBracket,
				Char::Whitespace | Char::Newline => {}
				Char::OpenCurly | Char::Percent | Char::Other(..) => {
					panic_at_location(
						&format!("Unexpected non-whitespace character \"{}\" when looking for value end curly braces.",
							byte as char
						),
						&position,
						context,
					)
				}
			},
			State::TagStart => match c {
				Char::Percent | Char::OpenCurly | Char::CloseCurly => panic_at_location(
					&format!(
						"Unexpected character \"{}\" following tag start.",
						byte as char
					),
					&position,
					context,
				),
				Char::Whitespace | Char::Newline => {}
				Char::Other(..) => {
					current_identifier.push(byte);
					state = State::TagFunction;
				}
			},
			State::TagFunction => match c {
				Char::Percent | Char::OpenCurly | Char::CloseCurly => panic_at_location(
					&format!(
						"Unexpected character \"{}\" in function name.",
						byte as char
					),
					&position,
					context,
				),
				Char::Newline => panic_at_location(
					"Unexpected newline in the middle of function name.",
					&position,
					context,
				),
				Char::Whitespace => {
					assert!(!current_identifier.is_empty());
					queued_identifiers.push(
						String::from_utf8_lossy(&current_identifier)
							.to_string(),
					);
					current_identifier.clear();
					state = State::TagNextParameter
				}
				Char::Other(..) => current_identifier.push(byte),
			},
			State::TagNextParameter => match c {
				Char::Whitespace => {}
				Char::OpenCurly | Char::CloseCurly => panic_at_location(
					&format!(
						"Unexpected character \"{}\" when looking for next parameter.",
						byte as char
					),
					&position,
					context,
				),
				Char::Percent | Char::Newline => {
					assert!(current_identifier.is_empty());
					let function = &queued_identifiers[0];
					let parameters = &queued_identifiers[1..];

					run_function(
						input_file,
						&mut output_buf,
						function,
						parameters,
						&mut loop_stack,
						skipping,
						context,
					);

					current_identifier.clear();
					queued_identifiers.clear();

					state = match c {
						Char::Percent => State::WaitingForCloseBracket,
						Char::Newline => State::TagEnd,
						_ => panic!("WTF?"),
					}
				}
				Char::Other(..) => {
					current_identifier.push(byte);
					state = State::TagInParameter
				}
			},
			State::TagInParameter => match c {
				Char::Whitespace => {
					assert!(!current_identifier.is_empty());
					queued_identifiers.push(
						String::from_utf8_lossy(&current_identifier)
							.to_string(),
					);
					current_identifier.clear();
					state = State::TagNextParameter;
				}
				Char::Newline => {
					if !current_identifier.is_empty() {
						queued_identifiers.push(
							String::from_utf8_lossy(&current_identifier)
								.to_string(),
						);
					}
					let function = &queued_identifiers[0];
					let parameters = &queued_identifiers[1..];

					run_function(
						input_file,
						&mut output_buf,
						function,
						parameters,
						&mut loop_stack,
						skipping,
						context,
					);

					current_identifier.clear();
					queued_identifiers.clear();

					state = State::WaitingForCloseBracket
				}
				Char::OpenCurly
				| Char::CloseCurly
				| Char::Percent
				| Char::Other(..) => current_identifier.push(byte),
			},
			State::TagEnd => match c {
				Char::Percent => state = State::WaitingForCloseBracket,
				Char::Whitespace | Char::Newline => {}
				Char::OpenCurly | Char::CloseCurly | Char::Other(..) => {
					panic_at_location(
						&format!(
							"Unexpected character \"{}\" when looking for tag end.",
							byte as char
						),
						&position,
						context,
					)
				}
			},
			State::WaitingForCloseBracket => match c {
				Char::CloseCurly => state = State::RegularContent,
				Char::OpenCurly
				| Char::Percent
				| Char::Newline
				| Char::Whitespace
				| Char::Other(..) => panic_at_location(
					"Missing close-bracket.",
					&position,
					context,
				),
			},
		}
		match c {
			Char::Newline => {
				position.line += 1;
				position.column = 1;
			}
			_ => position.column += 1,
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

fn panic_at_location(
	message: &str,
	position: &Position,
	context: &Context,
) -> ! {
	panic!(
		"{} Location: {}:{}:{}.",
		message,
		context.input_file_path.display(),
		position.line,
		position.column
	)
}

fn output_template_value(
	mut output_buf: &mut BufWriter<Vec<u8>>,
	identifiers: &[String],
	loop_stack: &[LoopInfo],
	context: &Context,
) {
	if identifiers.is_empty() {
		panic!("Encountered empty template value section, missing name.")
	}
	let name = &identifiers[0];
	let mut value = match fetch_template_value(name, loop_stack, context) {
		Value::Scalar(s) => s,
		Value::List(..) => panic!(
			"Cannot output list value {} directly, maybe use a for-loop?",
			name
		),
		Value::Dictionary(..) => {
			panic!("Cannot output dictionary value {} directly.", name)
		}
	};

	let mut offset = 1;
	while identifiers.len() > offset {
		if identifiers[offset] != "|" {
			panic!("Expected filter operator \"|\" as second identifier but got \"{}\".", identifiers[1]);
		}

		if identifiers.len() < offset + 1 {
			panic!("Missing filter function after filter operator \"|\".");
		}

		let filter_function = &identifiers[offset + 1];
		match filter_function.borrow() {
			"downcase" => {
				value = value.to_lowercase();
				offset += 2;
			}
			"upcase" => {
				value = value.to_uppercase();
				offset += 2;
			}
			_ => panic!("Unhandled filter function: {}", filter_function),
		}
	}
	write_to_stream(value.as_bytes(), &mut output_buf)
}

fn fetch_template_value(
	name: &str,
	loop_stack: &[LoopInfo],
	context: &Context,
) -> Value {
	let name_parts: Vec<&str> = name.split('.').collect();
	match name_parts.len() {
		1 => fetch_value(name, loop_stack, context),
		2 => fetch_field(name_parts[0], name_parts[1], loop_stack, context),
		_ => panic!("Unexpected identifier: {}", name),
	}
}

fn fetch_field(
	object: &str,
	field: &str,
	loop_stack: &[LoopInfo],
	context: &Context,
) -> Value {
	if object.is_empty() {
		panic!("Empty object name.")
	}
	if field.is_empty() {
		panic!("Empty field name.")
	}

	if object == "page" {
		match field {
			"content" => {
				if let Some(content) = context.html_content {
					Value::Scalar(String::from(content))
				} else {
					panic!("Requested content but none exists")
				}
			}
			"date" => Value::Scalar(
				context
					.front_matter
					.date
					.as_ref()
					.map_or_else(String::new, String::clone),
			),
			"title" => Value::Scalar(context.front_matter.title.clone()),
			"published" => {
				Value::Scalar(String::from(if context.front_matter.published {
					"true"
				} else {
					"false"
				}))
			}
			"edited" => Value::Scalar(
				context
					.front_matter
					.edited
					.as_ref()
					.map_or_else(String::new, String::clone),
			),
			// TODO: categories
			// TODO: tags
			// TODO: layout
			_ => {
				if let Some(value) =
					context.front_matter.custom_attributes.get(field)
				{
					Value::Scalar(value.to_string())
				} else {
					panic!("Not yet supported field: {}.{}", object, field)
				}
			}
		}
	} else {
		for loop_element in loop_stack.iter().rev() {
			if loop_element.variable == object {
				let value = &loop_element.values[loop_element.index];
				match value {
					Value::Dictionary(dict) => {
						return dict
							.map
							.get(field)
							.unwrap_or_else(|| {
								panic!(
									"Unhandled field \"{}.{}\"",
									object, field
								)
							})
							.clone();
					}
					_ => panic!(
						"Unexpected type of value in \"{}(.{})\": {:?}",
						object, field, value
					),
				}
			}
		}

		panic!("Unhandled object \"{}\"", object)
	}
}

fn fetch_value(
	name: &str,
	loop_stack: &[LoopInfo],
	context: &Context,
) -> Value {
	if let Some(entries) = context.groups.get(name) {
		let mut result = Vec::new();
		for entry in entries {
			let mut map = HashMap::new();
			map.insert(
				"title",
				Value::Scalar(entry.front_matter.title.clone()),
			);
			map.insert(
				"date",
				Value::Scalar(
					entry
						.front_matter
						.date
						.as_ref()
						.map_or_else(String::new, String::clone),
				),
			);
			map.insert(
				"link",
				Value::Scalar(make_relative_link(
					context.output_file_path,
					&entry.path,
					context.root_output_dir,
				)),
			);

			result.push(Value::Dictionary(Dictionary { map }))
		}
		return Value::List(List { values: result });
	}

	for loop_element in loop_stack.iter().rev() {
		if loop_element.variable == name {
			return loop_element.values[loop_element.index].clone();
		}
	}

	panic!("Failed finding value for \"{}\"", name);
}

fn run_function<T: Read + Seek>(
	input_file: &mut BufReader<T>,
	mut output_buf: &mut BufWriter<Vec<u8>>,
	function: &str,
	parameters: &[String],
	loop_stack: &mut Vec<LoopInfo>,
	skipping: bool,
	context: &Context,
) {
	if function.is_empty() {
		panic!("Empty function name.")
	}

	match function {
		"for" => {
			start_for(input_file, parameters, loop_stack, skipping, context)
		}
		"endfor" => {
			if !parameters.is_empty() {
				panic!(
					"Expecting no parameters to endfor. Encountered: {:?}",
					parameters
				)
			}

			end_for(input_file, loop_stack)
		}
		"include" => {
			if parameters.len() != 1 {
				panic!("Expecting at least 3 parameters (x in y) in for-loop. Encountered: {:?}", parameters)
			}
			if !skipping {
				let parameter = &parameters[0];
				include_file(&mut output_buf, parameter, context)
			}
		}
		"link" => {
			if parameters.len() != 1 {
				panic!("Expecting at least 3 parameters (x in y) in for-loop. Encountered: {:?}", parameters)
			}
			if !skipping {
				let parameter = &parameters[0];
				check_and_emit_link(
					context.output_file_path,
					&mut output_buf,
					parameter,
					context.root_input_dir,
					context.root_output_dir,
					context.input_output_map,
				)
			}
		}
		_ => panic!("Unsupported function: {}", function),
	}
}

fn start_for<T: Read + Seek>(
	input_file: &mut BufReader<T>,
	parameters: &[String],
	loop_stack: &mut Vec<LoopInfo>,
	skipping: bool,
	context: &Context,
) {
	if parameters.len() < 3 {
		panic!("Expecting at least 3 parameters (x in y) in for-loop. Encountered: {:?}", parameters)
	}

	let variable = &parameters[0];

	if parameters[1] != "in" {
		panic!("Expected for .. in ..");
	}

	let loop_values_name = &parameters[2];
	let loop_values: Vec<Value> = if skipping {
		Vec::new()
	} else {
		match fetch_template_value(loop_values_name, loop_stack, context) {
			Value::Scalar(s) => {
				s.chars().map(|c| Value::Scalar(c.to_string())).collect()
			}
			Value::List(l) => l.values,
			Value::Dictionary(dict) => dict.map.values().cloned().collect(),
		}
	};

	let limit = if parameters.len() > 3 {
		if parameters.len() != 5 {
			panic!("Expected 3 or 5 parameters to for loop of the form \"for .. in .. limit ..\", but got {} parameters.", parameters.len());
		}

		if parameters[3] != "limit" {
			panic!(
				"Expected 4th parameter to for loop to be \"limit\" but got {}",
			);
		}

		let limit_str = &parameters[4];
		limit_str.parse::<usize>().unwrap_or_else(|e| {
			panic!("Failed converting {} to usize: {}", limit_str, e)
		})
	} else {
		0
	};

	let end = if limit == 0 {
		loop_values.len()
	} else {
		min(loop_values.len(), limit)
	};

	loop_stack.push(LoopInfo {
		end,
		values: loop_values,
		variable: variable.to_string(),
		index: 0,
		buffer_start_position: input_file
			.seek(SeekFrom::Current(0))
			.unwrap_or_else(|e| {
				panic!(
					"Failed fetching current position from input stream: {}",
					e
				)
			}),
	})
}

fn end_for<T: Read + Seek>(
	input_file: &mut BufReader<T>,
	loop_stack: &mut Vec<LoopInfo>,
) {
	if loop_stack.is_empty() {
		panic!("Encountered endfor without preceding for.");
	}
	let last_index = loop_stack.len() - 1;
	let mut loop_info = &mut loop_stack[last_index];
	loop_info.index += 1;
	if loop_info.index < loop_info.end {
		input_file
			.seek(SeekFrom::Start(loop_info.buffer_start_position))
			.unwrap_or_else(|e| {
				panic!(
					"Failed seeking to position {} in input stream: {}",
					loop_info.buffer_start_position, e
				)
			});
	} else {
		loop_stack.pop();
	}
}

fn include_file(
	mut output_buf: &mut BufWriter<Vec<u8>>,
	parameter: &str,
	context: &Context,
) {
	let included_file_path =
		context.root_input_dir.join("_includes").join(&*parameter);

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
		&Context {
			input_file_path: &included_file_path,
			..*context
		},
	)
}

fn check_and_emit_link(
	output_file_path: &PathBuf,
	output_buf: &mut BufWriter<Vec<u8>>,
	parameter: &str,
	root_input_dir: &PathBuf,
	root_output_dir: &PathBuf,
	input_output_map: &HashMap<PathBuf, OptionOutputFile>,
) {
	let append_index_html = parameter.ends_with('/');
	if !parameter.starts_with('/') {
		panic!(
			"Only absolute paths are allowed in links, but got: {}",
			parameter
		);
	}
	let mut path = root_input_dir.join(PathBuf::from(&parameter[1..]));
	if append_index_html {
		path = path.join(PathBuf::from("index.html"));
	}

	let linked_output_path = &match input_output_map.get(&path) {
		Some(lo) => lo,
		_ => panic!(
			"Failed finding {} among: {:#?}",
			path.display(),
			input_output_map.keys()
		),
	}
	.path;

	write_to_stream(
		make_relative_link(
			output_file_path,
			linked_output_path,
			root_output_dir,
		)
		.as_bytes(),
		output_buf,
	);
}

fn make_relative_link(
	output_file_path: &PathBuf,
	linked_output_path: &PathBuf,
	root_output_dir: &PathBuf,
) -> std::string::String {
	let mut equal_prefix = PathBuf::new();
	let mut equal_component_count = 0;
	for (self_component, link_component) in output_file_path
		.components()
		.zip(linked_output_path.components())
	{
		if self_component != link_component {
			break;
		}
		equal_prefix = equal_prefix.join(self_component);
		equal_component_count += 1;
	}
	if equal_prefix.iter().next() == None {
		panic!("No common prefix, expected at least {} but own path is {} and link is {}.", root_output_dir.display(), output_file_path.display(), linked_output_path.display());
	}

	assert!(
		output_file_path.starts_with(root_output_dir),
		"Expected {} to start with {}.",
		output_file_path.display(),
		root_output_dir.display()
	);

	// Do not strip own file name from link if path is the same.
	if output_file_path == linked_output_path {
		equal_prefix.pop();
	}

	let own_component_count = output_file_path.components().count();
	let linked_component_count = linked_output_path.components().count();
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
		linked_output_path
			.strip_prefix(&prefix_plus_slash)
			.unwrap_or_else(|e| {
				panic!(
					"Failed stripping prefix {} from {}: {}",
					prefix_plus_slash,
					linked_output_path.display(),
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

	println!(
		"File: {}, translated link: {}, prefix+slash: {}, result: {}",
		output_file_path.display(),
		linked_output_path.display(),
		prefix_plus_slash,
		&linked_output_path_stripped_str
	);

	linked_output_path_stripped_str
}
