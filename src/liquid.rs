use core::cmp::min;
use std::borrow::Borrow;
use std::collections::HashMap;
use std::convert::TryInto;
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

#[derive(Clone, Debug, PartialEq)]
struct List {
	values: Vec<Value>,
}

#[derive(Clone, Debug, PartialEq)]
struct Dictionary {
	map: HashMap<&'static str, Value>,
}

#[derive(Clone, Debug, PartialEq)]
enum Value {
	String(String),
	Integer(i32),
	List(List),
	Dictionary(Dictionary),
}

enum ControlFlow {
	For {
		values: Vec<Value>,
		variable: String,
		index: usize,
		end: usize,
		buffer_start_position: u64,
	},
	If {
		condition: bool,
	},
}

impl ControlFlow {
	fn if_new(condition: bool) -> ControlFlow {
		ControlFlow::If { condition }
	}
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
		Quote,
		Other(u8),
	}

	let mut state = State::RegularContent;
	let mut position = Position { line: 1, column: 1 };
	let mut current_identifier: Vec<u8> = Vec::new();
	let mut parsing_literal = false;
	let mut queued_identifiers: Vec<String> = Vec::new();
	let mut cf_stack: Vec<ControlFlow> = Vec::new();

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
			b'"' => Char::Quote,
			_ => Char::Other(byte),
		};

		let skipping = cf_stack.last().map_or(false, |cf| match cf {
			ControlFlow::For { values, .. } => values.is_empty(),
			ControlFlow::If { condition, .. } => !*condition,
		});

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
				| Char::Quote
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
							&cf_stack,
							context,
						);
					}

					current_identifier.clear();
					parsing_literal = false;
					queued_identifiers.clear();
					state = match c {
						Char::CloseCurly => State::WaitingForCloseBracket,
						Char::Newline => State::ValueEnd,
						_ => panic!("WTF?"),
					}
				}
				Char::Whitespace => {}
				Char::Quote => {
					parsing_literal = true;
					current_identifier.push(byte);
					state = State::ValueInIdentifier;
				}
				Char::Percent | Char::Other(..) => {
					current_identifier.push(byte);
					state = State::ValueInIdentifier;
				}
			},
			State::ValueInIdentifier => if parsing_literal {
				if current_identifier.len() > 1 && current_identifier.last() == Some(&b'"') {
					match c {
						Char::Whitespace | Char::Newline => {
							queued_identifiers.push(String::from_utf8_lossy(&current_identifier).to_string());
							current_identifier.clear();
							parsing_literal = false;
							state = State::ValueNextIdentifier
						},
						Char::Percent | Char::OpenCurly | Char::CloseCurly | Char::Quote | Char::Other(..) => {
							panic!("Already had 2 quotes in string literal: {}", String::from_utf8_lossy(&current_identifier))
						}
					}
				} else {
					current_identifier.push(byte)
				}
			} else {
				match c {
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
									&cf_stack,
									context,
								)
							}
							current_identifier.clear();
							queued_identifiers.clear();
							state = State::ValueEnd
						}
					}
					Char::Quote => panic_at_location(
						"Unexpected quote (\") in the middle of non-literal.",
						&position,
						context,
					),
					Char::OpenCurly | Char::Percent | Char::Other(..) => {
						current_identifier.push(byte)
					}
				}
			},
			State::ValueEnd => match c {
				Char::CloseCurly => state = State::WaitingForCloseBracket,
				Char::Whitespace | Char::Newline => {}
				Char::OpenCurly | Char::Percent | Char::Quote | Char::Other(..) => panic_at_location(
					&format!("Unexpected non-whitespace character \"{}\" when looking for value end curly braces.",
						byte as char
					),
					&position,
					context,
				)
			},
			State::TagStart => match c {
				Char::Percent | Char::OpenCurly | Char::CloseCurly => panic_at_location(
					&format!(
						"Unexpected character \"{}\" following tag start when expecting function name.",
						byte as char
					),
					&position,
					context,
				),
				Char::Quote => panic_at_location(
					"Unexpected quote (\") following tag start when expecting function name.",
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
				Char::Quote => panic_at_location(
					"Unexpected quote (\") in the middle of function name.",
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
					assert!(!parsing_literal);
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
						&mut cf_stack,
						skipping,
						context,
					);

					current_identifier.clear();
					assert!(!parsing_literal);
					queued_identifiers.clear();

					state = match c {
						Char::Percent => State::WaitingForCloseBracket,
						Char::Newline => State::TagEnd,
						_ => panic!("WTF?"),
					}
				}
				Char::Quote => {
					current_identifier.push(byte);
					parsing_literal = true;
					state = State::TagInParameter
				}
				Char::Other(..) => {
					current_identifier.push(byte);
					state = State::TagInParameter
				}
			},
			State::TagInParameter => if parsing_literal {
				if current_identifier.len() > 1 && current_identifier.last() == Some(&b'"') {
					match c {
						Char::Whitespace | Char::Newline => {
							queued_identifiers.push(String::from_utf8_lossy(&current_identifier).to_string());
							current_identifier.clear();
							parsing_literal = false;
							state = State::TagNextParameter
						},
						Char::Percent | Char::OpenCurly | Char::CloseCurly | Char::Quote | Char::Other(..) => {
							panic!("Already had 2 quotes in string literal ({}) when encountering: {}", String::from_utf8_lossy(&current_identifier), byte as char)
						}
					}
				} else {
					current_identifier.push(byte)
				}
			} else {
				match c {
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
							&mut cf_stack,
							skipping,
							context,
						);

						current_identifier.clear();
						queued_identifiers.clear();

						state = State::WaitingForCloseBracket
					}
					Char::Quote => panic_at_location(
						"Unexpected quote (\") in the middle of non-literal.",
						&position,
						context,
					),
					Char::OpenCurly
					| Char::CloseCurly
					| Char::Percent
					| Char::Other(..) => current_identifier.push(byte),
				}
			},
			State::TagEnd => match c {
				Char::Percent => state = State::WaitingForCloseBracket,
				Char::Whitespace | Char::Newline => {}
				Char::OpenCurly | Char::CloseCurly | Char::Quote | Char::Other(..) => {
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
				| Char::Quote
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
	cf_stack: &[ControlFlow],
	context: &Context,
) {
	if identifiers.is_empty() {
		panic!("Encountered empty template value section, missing name.")
	}
	let name = &identifiers[0];
	let mut value = match fetch_template_value(name, cf_stack, context) {
		Value::String(s) => s,
		Value::Integer(i) => i.to_string(),
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
			"date" => {
				if identifiers.len() < offset + 2 {
					panic!("date filter function requires a parameter.");
				}
				let format_string = match fetch_template_value(
					&identifiers[offset + 2],
					cf_stack,
					context,
				) {
					Value::String(s) => s,
					_ => panic!(
						"Cannot handle non-string value as format string."
					),
				};

				const EXAMPLE_DATETIME: &str = "2001-01-19T20:10:01Z";
				if value.len() != EXAMPLE_DATETIME.len() {
					panic!("date filter requires valid date format such as {}, but got: {}", EXAMPLE_DATETIME, value);
				}
				let mut special = false;
				let mut result = String::new();
				for c in format_string.chars() {
					if c == '%' {
						special = !special;
					} else if special {
						// TODO: Verify this is standard..
						match c {
							'Y' => result.push_str(&value[0..4]),
							'y' => result.push_str(&value[2..4]),
							'm' => result.push_str(&value[5..7]),
							'b' | 'h' => {
								let num_month = &value[5..7];
								result.push_str(match num_month {
									"01" => "Jan",
									"02" => "Feb",
									"03" => "Mar",
									"04" => "Apr",
									"05" => "May",
									"06" => "Jun",
									"07" => "Jul",
									"08" => "Aug",
									"09" => "Sep",
									"10" => "Oct",
									"11" => "Nov",
									"12" => "Dec",
									_ => panic!("Failed converting {} into month string, expected number between 01-12", num_month)
								});
							}
							'B' => {
								let num_month = &value[5..7];
								result.push_str(match num_month {
									"01" => "January",
									"02" => "February",
									"03" => "March",
									"04" => "April",
									"05" => "May",
									"06" => "June",
									"07" => "July",
									"08" => "August",
									"09" => "September",
									"10" => "October",
									"11" => "November",
									"12" => "December",
									_ => panic!("Failed converting {} into month string, expected number between 01-12", num_month)
								});
							}
							'd' => result.push_str(&value[8..10]),
							'H' => result.push_str(&value[11..13]),
							'M' => result.push_str(&value[14..16]),
							'S' => result.push_str(&value[17..19]),
							_ => panic!("Unhandled special character: {}", c),
						}
						special = false
					} else {
						result.push(c)
					}
				}
				value = result;
				offset += 3
			}
			"downcase" => {
				value = value.to_lowercase();
				offset += 2
			}
			"upcase" => {
				value = value.to_uppercase();
				offset += 2
			}
			_ => panic!("Unhandled filter function: {}", filter_function),
		}
	}
	write_to_stream(value.as_bytes(), &mut output_buf)
}

fn fetch_template_value(
	name: &str,
	cf_stack: &[ControlFlow],
	context: &Context,
) -> Value {
	assert!(!name.is_empty(), "Never expected to get empty identifiers.");

	if name.len() > 1 && name.starts_with('"') && name.ends_with('"') {
		return Value::String(name[1..name.len() - 1].to_string());
	}

	{
		let numeric_offset = if name.chars().next() == Some('-') {
			1
		} else {
			0
		};
		if name[numeric_offset..].chars().all(|c| c.is_digit(10)) {
			return Value::Integer(name.parse::<i32>().unwrap_or_else(|e| {
				panic!("Failed converting {} to an i32: {}", name, e)
			}));
		}
	}

	let name_parts: Vec<&str> = name.split('.').collect();
	match name_parts.len() {
		1 => fetch_value(name, cf_stack, context),
		2 => fetch_field(name_parts[0], name_parts[1], cf_stack, context),
		_ => panic!("Unexpected identifier: {}", name),
	}
}

fn fetch_field(
	object: &str,
	field: &str,
	cf_stack: &[ControlFlow],
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
					Value::String(String::from(content))
				} else {
					panic!("Requested content but none exists")
				}
			}
			"date" => Value::String(
				context
					.front_matter
					.date
					.as_ref()
					.map_or_else(String::new, String::clone),
			),
			"title" => Value::String(context.front_matter.title.clone()),
			"published" => {
				Value::String(String::from(if context.front_matter.published {
					"true"
				} else {
					"false"
				}))
			}
			"edited" => Value::String(
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
					Value::String(value.to_string())
				} else {
					panic!("Not yet supported field: {}.{}", object, field)
				}
			}
		}
	} else {
		for cf in cf_stack.iter().rev() {
			match cf {
				ControlFlow::For {
					variable,
					values,
					index,
					..
				} => {
					if variable == object {
						let value = &values[*index];
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
				ControlFlow::If { .. } => {}
			}
		}

		if let Some(entries) = context.groups.get(object) {
			match field {
				"count" => {
					return Value::Integer(
						entries.len().try_into().unwrap_or_else(|e| {
							panic!(
								"Failed converting {}.{} with value {} to i32: {}",
								object,
								field,
								entries.len(),
								e
							)
						}),
					)
				}
				_ => panic!("Unhandled field {} on object {}.", field, object),
			}
		}

		panic!("Unhandled object \"{}\"", object)
	}
}

fn fetch_value(
	name: &str,
	cf_stack: &[ControlFlow],
	context: &Context,
) -> Value {
	if let Some(entries) = context.groups.get(name) {
		let mut result = Vec::new();
		for entry in entries {
			let mut map = HashMap::new();
			map.insert(
				"title",
				Value::String(entry.front_matter.title.clone()),
			);
			map.insert(
				"date",
				Value::String(
					entry
						.front_matter
						.date
						.as_ref()
						.map_or_else(String::new, String::clone),
				),
			);
			map.insert(
				"link",
				Value::String(make_relative_link(
					context.output_file_path,
					&entry.path,
					context.root_output_dir,
				)),
			);

			result.push(Value::Dictionary(Dictionary { map }))
		}
		return Value::List(List { values: result });
	}

	for cf in cf_stack.iter().rev() {
		match cf {
			ControlFlow::For {
				variable,
				values,
				index,
				..
			} => {
				if variable == name {
					return values[*index].clone();
				}
			}
			ControlFlow::If { .. } => {}
		}
	}

	panic!("Failed finding value for \"{}\"", name);
}

fn run_function<T: Read + Seek>(
	input_file: &mut BufReader<T>,
	mut output_buf: &mut BufWriter<Vec<u8>>,
	function: &str,
	parameters: &[String],
	cf_stack: &mut Vec<ControlFlow>,
	skipping: bool,
	context: &Context,
) {
	if function.is_empty() {
		panic!("Empty function name.")
	}

	match function {
		"if" => start_if(parameters, cf_stack, skipping, context),
		"endif" => end_if(parameters, cf_stack),
		"for" => start_for(input_file, parameters, cf_stack, skipping, context),
		"endfor" => end_for(input_file, parameters, cf_stack),
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

fn start_if(
	parameters: &[String],
	cf_stack: &mut Vec<ControlFlow>,
	skipping: bool,
	context: &Context,
) {
	if skipping {
		cf_stack.push(ControlFlow::if_new(false));
		return;
	}

	if parameters.is_empty() {
		panic!("if-statement lacks conditional expression.")
	}

	if parameters.len() != 3 {
		panic!("Unsupported conditional expression: {:?}", parameters)
	}

	let lhs = fetch_template_value(&parameters[0], cf_stack, context);
	let rhs = fetch_template_value(&parameters[2], cf_stack, context);

	match parameters[1].borrow() {
		"==" => cf_stack.push(ControlFlow::if_new(lhs == rhs)),
		"!=" => cf_stack.push(ControlFlow::if_new(lhs != rhs)),
		"<" => {
			if let (Value::Integer(l), Value::Integer(r)) = (&lhs, &rhs) {
				cf_stack.push(ControlFlow::if_new(l < r))
			} else {
				panic!(
					"Expecting to compare integer types but got {:?} and {:?}",
					lhs, rhs
				)
			}
		}
		">" => {
			if let (Value::Integer(l), Value::Integer(r)) = (&lhs, &rhs) {
				cf_stack.push(ControlFlow::if_new(l > r))
			} else {
				panic!(
					"Expecting to compare integer types but got {:?} and {:?}",
					lhs, rhs
				)
			}
		}
		"<=" => {
			if let (Value::Integer(l), Value::Integer(r)) = (&lhs, &rhs) {
				cf_stack.push(ControlFlow::if_new(l <= r))
			} else {
				panic!(
					"Expecting to compare integer types but got {:?} and {:?}",
					lhs, rhs
				)
			}
		}
		">=" => {
			if let (Value::Integer(l), Value::Integer(r)) = (&lhs, &rhs) {
				cf_stack.push(ControlFlow::if_new(l >= r))
			} else {
				panic!(
					"Expecting to compare integer types but got {:?} and {:?}",
					lhs, rhs
				)
			}
		}
		_ => panic!("Unsupported operator: {:?}", parameters[1]),
	}
}

fn end_if(parameters: &[String], cf_stack: &mut Vec<ControlFlow>) {
	if !parameters.is_empty() {
		panic!(
			"Expecting no parameters to endif. Encountered: {:?}",
			parameters
		)
	}

	if cf_stack.is_empty() {
		panic!("Encountered endif without preceding if.");
	}
	let last_index = cf_stack.len() - 1;
	let cf = &mut cf_stack[last_index];
	match cf {
		ControlFlow::If { .. } => {
			cf_stack.pop();
		}
		_ => panic!("Encountered endif without match preceeding if."),
	}
}

fn start_for<T: Read + Seek>(
	input_file: &mut BufReader<T>,
	parameters: &[String],
	cf_stack: &mut Vec<ControlFlow>,
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
		match fetch_template_value(loop_values_name, cf_stack, context) {
			Value::String(s) => {
				s.chars().map(|c| Value::String(c.to_string())).collect()
			}
			Value::Integer(..) => {
				panic!("Cannot iterate over one single integer value.")
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

	cf_stack.push(ControlFlow::For {
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
	parameters: &[String],
	cf_stack: &mut Vec<ControlFlow>,
) {
	if !parameters.is_empty() {
		panic!(
			"Expecting no parameters to endfor. Encountered: {:?}",
			parameters
		)
	}

	if cf_stack.is_empty() {
		panic!("Encountered endfor without preceding for.");
	}
	let last_index = cf_stack.len() - 1;
	let cf = &mut cf_stack[last_index];
	match cf {
		ControlFlow::For {
			index,
			end,
			buffer_start_position,
			..
		} => {
			*index += 1;
			if index < end {
				input_file
					.seek(SeekFrom::Start(*buffer_start_position))
					.unwrap_or_else(|e| {
						panic!(
							"Failed seeking to position {} in input stream: {}",
							buffer_start_position, e
						)
					});
			} else {
				cf_stack.pop();
			}
		}
		_ => panic!("Encountered endfor without match preceeding for."),
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
