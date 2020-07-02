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
	input_output_map: &HashMap<PathBuf, OptionOutputFile>,
	groups: &HashMap<String, Vec<OutputFile>>,
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
		TagNextParameter,
		TagInParameter,
		TagEnd,
		WaitingForCloseBracket,
	}

	let mut state = State::RegularContent;
	let mut buf = [0_u8; 1];
	let mut position = Position { line: 1, column: 1 };
	let mut object = Vec::new();
	let mut field = Vec::new();
	let mut function = Vec::new();
	let mut parameters: Vec<Vec<u8>> = Vec::new();
	let mut loop_stack: Vec<LoopInfo> = Vec::new();

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

		let byte = buf[0];
		let skipping = loop_stack
			.last()
			.map_or(false, |loop_info| loop_info.values.is_empty());

		if byte == b'\n' {
			match state {
				State::RegularContent => {
					write_to_stream(&[byte], output_buf);
				}
				State::LastOpenBracket => {
					if !skipping {
						write_to_stream(b"{\n", output_buf)
					}
					state = State::RegularContent
				}
				State::ValueObject => panic_at_location(
					"Unexpected newline while reading value object identifier.",
					position,
					context,
				),
				State::ValueField => {
					if !skipping {
						output_template_value(
							&mut output_buf,
							&object,
							&field,
							context.front_matter,
							context.html_content,
							&loop_stack,
						)
					}
					object.clear();
					field.clear();
					state = State::ValueEnd
				}
				State::WaitingForCloseBracket => panic_at_location(
					"Expected close bracket but got newline.",
					position,
					context,
				),
				State::TagFunction => panic_at_location(
					"Unexpected newline in the middle of function.",
					position,
					context,
				),
				State::TagNextParameter | State::TagInParameter => {
					run_function(
						input_file,
						&mut output_buf,
						&mut function,
						&mut parameters,
						input_output_map,
						groups,
						&mut loop_stack,
						context,
					);
					state = State::TagEnd
				}
				State::ValueStart
				| State::ValueEnd
				| State::TagStart
				| State::TagEnd => {}
			}
			position.line += 1;
			position.column = 1;
		} else {
			match state {
				State::RegularContent => match byte {
					b'{' => state = State::LastOpenBracket,
					_ => {
						if !skipping {
							write_to_stream(&[byte], output_buf)
						}
					}
				},
				State::LastOpenBracket => match byte {
					b'{' => state = State::ValueStart,
					b'%' => state = State::TagStart,
					_ => {
						if !skipping {
							write_to_stream(&[b'{'], output_buf)
						}
						state = State::RegularContent;
					}
				},
				State::ValueStart => match byte {
					b'{' => panic_at_location(
						"Unexpected open bracket while in template mode.",
						position,
						context,
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
						position,
						context,
					),
					b'}' => state = State::WaitingForCloseBracket,
					b' ' | b'\t' => {
						if !skipping {
							output_template_value(
								&mut output_buf,
								&object,
								&field,
								context.front_matter,
								context.html_content,
								&loop_stack,
							);
						}
						object.clear();
						field.clear();
						state = State::ValueEnd
					}
					_ => field.push(byte),
				},
				State::ValueEnd => match byte {
					b'}' => state = State::WaitingForCloseBracket,
					b' ' | b'\t' => {}
					_ => panic_at_location(
						"Unexpected non-whitespace character.",
						position,
						context,
					),
				},
				State::TagStart => match byte {
					b'%' => panic_at_location(
						"Unexpected % following tag start.",
						position,
						context,
					),
					b' ' | b'\t' => {}
					_ => {
						function.push(byte);
						state = State::TagFunction;
					}
				},
				State::TagFunction => match byte {
					b' ' | b'\t' => state = State::TagNextParameter,
					_ => function.push(byte),
				},
				State::TagNextParameter => match byte {
					b' ' | b'\t' => {}
					b'%' => {
						run_function(
							input_file,
							&mut output_buf,
							&mut function,
							&mut parameters,
							input_output_map,
							groups,
							&mut loop_stack,
							context,
						);
						state = State::WaitingForCloseBracket
					}
					_ => {
						parameters.push(vec![byte]);
						state = State::TagInParameter
					}
				},
				State::TagInParameter => {
					match byte {
						b' ' | b'\t' => state = State::TagNextParameter,
						b'%' => {
							run_function(
								input_file,
								&mut output_buf,
								&mut function,
								&mut parameters,
								input_output_map,
								groups,
								&mut loop_stack,
								context,
							);
							state = State::WaitingForCloseBracket
						}
						_ => {
							if let Some(last) = parameters.last_mut() {
								last.push(byte)
							} else {
								panic!("Should not be in {:?} without any parameters.", state);
							}
						}
					}
				}
				State::TagEnd => match byte {
					b'%' => state = State::WaitingForCloseBracket,
					b' ' | b'\t' => {}
					_ => panic_at_location(
						&format!(
							"Unexpected non-whitespace character: \"{}\".",
							byte as char
						),
						position,
						context,
					),
				},
				State::WaitingForCloseBracket => {
					if byte == b'}' {
						state = State::RegularContent
					} else {
						panic_at_location(
							"Missing double close-bracket.",
							position,
							context,
						)
					}
				}
			}
			position.column += 1
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
	position: Position,
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
	object: &[u8],
	field: &[u8],
	front_matter: &FrontMatter,
	html_content: Option<&str>,
	loop_stack: &Vec<LoopInfo>,
) {
	match fetch_template_value(
		object,
		field,
		front_matter,
		html_content,
		loop_stack,
	) {
		Value::Scalar(s) => write_to_stream(s.as_bytes(), &mut output_buf),
		Value::List(..) => panic!(
			"Cannot output list value {}.{} directly, maybe use a for-loop?",
			String::from_utf8_lossy(object),
			String::from_utf8_lossy(field)
		),
		Value::Dictionary(..) => panic!(
			"Cannot output dictionary value {}.{} directly.",
			String::from_utf8_lossy(object),
			String::from_utf8_lossy(field)
		),
	}
}

fn fetch_template_value(
	object: &[u8],
	field: &[u8],
	front_matter: &FrontMatter,
	html_content: Option<&str>,
	loop_stack: &Vec<LoopInfo>,
) -> Value {
	fetch_template_value_str(
		&String::from_utf8_lossy(object),
		&String::from_utf8_lossy(field),
		front_matter,
		html_content,
		loop_stack,
	)
}

fn fetch_template_value_str(
	object: &str,
	field: &str,
	front_matter: &FrontMatter,
	html_content: Option<&str>,
	loop_stack: &Vec<LoopInfo>,
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
				if let Some(content) = html_content {
					return Value::Scalar(String::from(content));
				} else {
					panic!("Requested content but none exists")
				}
			}
			"date" => {
				return Value::Scalar(
					front_matter
						.date
						.as_ref()
						.map_or_else(|| String::new(), |s| s.clone()),
				)
			}
			"title" => return Value::Scalar(front_matter.title.clone()),
			"published" => {
				return Value::Scalar(String::from(if front_matter.published {
					"true"
				} else {
					"false"
				}))
			}
			"edited" => {
				return Value::Scalar(
					front_matter
						.edited
						.as_ref()
						.map_or_else(|| String::new(), |s| s.clone()),
				)
			}
			// TODO: categories
			// TODO: tags
			// TODO: layout
			_ => {
				if let Some(value) = front_matter.custom_attributes.get(field) {
					return Value::Scalar(value.to_string());
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
	output_file_path: &PathBuf,
	root_output_dir: &PathBuf,
	name: &str,
	groups: &HashMap<String, Vec<OutputFile>>,
	loop_stack: &Vec<LoopInfo>,
) -> Value {
	if let Some(entries) = groups.get(name) {
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
						.map_or_else(|| String::new(), |d| d.clone()),
				),
			);
			map.insert(
				"link",
				Value::Scalar(make_relative_link(
					output_file_path,
					&entry.path,
					root_output_dir,
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
	function: &mut Vec<u8>,
	parameters: &mut Vec<Vec<u8>>,
	input_output_map: &HashMap<PathBuf, OptionOutputFile>,
	groups: &HashMap<String, Vec<OutputFile>>,
	loop_stack: &mut Vec<LoopInfo>,
	context: &Context,
) {
	if function.is_empty() {
		panic!("Empty function name.")
	}

	let skipping = loop_stack
		.last()
		.map_or(false, |loop_info| loop_info.values.is_empty());

	let function_str = String::from_utf8_lossy(function);
	match function_str.borrow() {
		"for" => start_for(
			input_file, parameters, groups, loop_stack, skipping, context,
		),
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
				let parameter_str = String::from_utf8_lossy(&parameters[0]);
				include_file(
					&mut output_buf,
					&parameter_str,
					input_output_map,
					groups,
					context,
				)
			}
		}
		"link" => {
			if parameters.len() != 1 {
				panic!("Expecting at least 3 parameters (x in y) in for-loop. Encountered: {:?}", parameters)
			}
			if !skipping {
				let parameter_str = String::from_utf8_lossy(&parameters[0]);
				check_and_emit_link(
					context.output_file_path,
					&mut output_buf,
					&parameter_str,
					context.root_input_dir,
					context.root_output_dir,
					input_output_map,
				)
			}
		}
		_ => panic!("Unsupported function: {}", function_str),
	}
	function.clear();
	parameters.clear();
}

fn start_for<T: Read + Seek>(
	input_file: &mut BufReader<T>,
	parameters: &Vec<Vec<u8>>,
	groups: &HashMap<String, Vec<OutputFile>>,
	loop_stack: &mut Vec<LoopInfo>,
	skipping: bool,
	context: &Context,
) {
	if parameters.len() < 3 {
		panic!("Expecting at least 3 parameters (x in y) in for-loop. Encountered: {:?}", parameters)
	}

	let variable = String::from_utf8_lossy(&parameters[0]).to_string();

	if parameters[1] != b"in" {
		panic!("Expected for .. in ..");
	}

	let loop_values_name: Vec<&[u8]> =
		parameters[2].as_slice().split(|b| *b == b'.').collect();
	if loop_values_name.len() > 2 {
		panic!(
			"Expected variable or object.field loop value name, but got \"{}\", part count: {}",
			String::from_utf8_lossy(&parameters[2]),
			loop_values_name.len()
		);
	}
	let loop_values: Vec<Value> = if skipping {
		Vec::new()
	} else {
		let value = if loop_values_name.len() == 1 {
			fetch_value(
				context.output_file_path,
				context.root_output_dir,
				&String::from_utf8_lossy(loop_values_name[0]),
				groups,
				loop_stack,
			)
		} else if loop_values_name.len() == 2 {
			fetch_template_value(
				loop_values_name[0],
				loop_values_name[1],
				context.front_matter,
				context.html_content,
				loop_stack,
			)
		} else {
			panic!(
				"Unexpected variable name: {}",
				String::from_utf8_lossy(&parameters[2])
			)
		};

		match value {
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

		if parameters[3] != b"limit" {
			panic!(
				"Expected 4th parameter to for loop to be \"limit\" but got {}",
			);
		}

		let limit_str = String::from_utf8_lossy(&parameters[4]);
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
		variable,
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
	parameter_str: &str,
	input_output_map: &HashMap<PathBuf, OptionOutputFile>,
	groups: &HashMap<String, Vec<OutputFile>>,
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
		groups,
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
	input_output_map: &HashMap<PathBuf, OptionOutputFile>,
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
