use std::borrow::Borrow;
use std::cmp::min;
use std::collections::HashMap;
use std::convert::TryInto;
use std::ffi::OsStr;
use std::fs;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom};
use std::path::PathBuf;

use crate::front_matter::FrontMatter;
use crate::markdown::{GroupedOptionOutputFile, InputFile};
use crate::util::{strip_prefix, write_to_stream, SiteInfo};

pub struct Context<'a> {
	pub input_file_path: &'a PathBuf,
	pub output_file_path: &'a PathBuf,
	pub front_matter: &'a FrontMatter,
	pub html_content: Option<&'a str>,
	pub root_input_dir: &'a PathBuf,
	pub root_output_dir: &'a PathBuf,
	pub input_output_map: &'a HashMap<PathBuf, GroupedOptionOutputFile>,
	pub groups: &'a HashMap<String, Vec<InputFile>>,
	pub site_info: &'a SiteInfo<'a>,
}

#[derive(Clone)]
struct Position {
	line: usize,
	column: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
	Boolean(bool),
	String(String),
	Integer(i32),
	List { values: Vec<Value> },
	Dictionary { map: HashMap<&'static str, Value> },
}

impl Value {
	fn get_field(&self, field: &str) -> Self {
		if let Self::Dictionary { map } = self {
			if let Some(map_value) = map.get(field) {
				return map_value.clone();
			}
		}

		match field {
			"count" => {
				let length = match self {
					Self::String(s) => s.len(),
					Self::List { values } => values.len(),
					Self::Dictionary { map } => map.len(),
					Self::Boolean(..) | Self::Integer(..) => {
						panic!("Tried to get length of {:?}.", self)
					}
				};
				Self::Integer(length.try_into().unwrap_or_else(|e| {
					panic!(
						"Failed converting length {} of value {:?} to i32: {}",
						length, self, e
					)
				}))
			}
			_ => panic!("Failed getting field {} on: {:?}", field, self),
		}
	}

	fn string_content(&self) -> String {
		match self {
			Self::Boolean(b) => b.to_string(),
			Self::String(s) => s.clone(),
			Self::Integer(i) => i.to_string(),
			Self::Dictionary { .. } | Self::List { .. } => panic!(
				"Conversion to string for this type of value is not supported: {:?}",
				self
			),
		}
	}
}

#[derive(Debug)]
enum ControlFlow {
	For {
		values: Vec<Value>,
		variable: String,
		index: usize,
		end: usize,
		buffer_start_position: u64,
		local_variables: HashMap<String, Value>,
	},
	If {
		condition: bool,
		local_variables: HashMap<String, Value>,
	},
	Capture {
		variable: String,
		content: Vec<u8>,
	},
}

impl ControlFlow {
	fn if_new(condition: bool) -> Self {
		Self::If {
			condition,
			local_variables: HashMap::new(),
		}
	}
}

// Rolling a simple version of Liquid parsing on my own since the official Rust
// one has too many dependencies.
//
// Allowing more lines to keep state machine cohesive.
#[allow(clippy::too_many_lines)]
pub fn process<T: Read + Seek>(
	input_file: &mut BufReader<T>,
	mut parent_output_buf: &mut BufWriter<Vec<u8>>,
	mut outer_variables: HashMap<String, Value>,
	context: &Context,
) {
	#[derive(Debug)]
	enum State {
		RegularContent,
		LastOpenBracket,
		ValueWaitingForDashOrWhitespace,
		ValueNextIdentifier,
		ValueEncounteredDashInIdentifierOrEnd,
		ValueInIdentifier,
		ValueEnd,
		ValueWaitingForFirstCloseBracket,
		TagWaitingForDashOrWhitespace,
		TagStart,
		TagFunction,
		TagNextParameter,
		TagEncounteredDashInParameterOrEnd,
		TagInParameter,
		TagEnd,
		WaitingForPercent,
		WaitingForCloseBracket,
	}

	enum Char {
		OpenCurly,
		CloseCurly,
		Percent,
		Newline,
		Whitespace,
		Quote,
		Dash,
		Other(u8),
	}

	let mut state = State::RegularContent;
	let mut position = Position {
		line: context.front_matter.subsequent_line,
		column: 1,
	};
	let mut current_identifier: Vec<u8> = Vec::new();
	let mut parsing_literal = false;
	let mut queued_identifiers: Vec<String> = Vec::new();
	let mut cf_stack: Vec<ControlFlow> = Vec::new();
	let mut capture_buf = BufWriter::new(Vec::new());
	let mut buffered_whitespace = Vec::new();
	let mut removing_whitespace = false;

	let flush_whitespace = |from: &mut Vec<u8>, to: &mut BufWriter<Vec<u8>>| {
		if !from.is_empty() {
			write_to_stream(from, to);
			from.clear()
		}
	};

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
			b'-' => Char::Dash,
			_ => Char::Other(byte),
		};

		let (skipping, capture_index) = {
			let mut s = None;
			let mut ci = usize::max_value();
			for (index, cf) in cf_stack.iter().rev().enumerate() {
				match cf {
					ControlFlow::For { values, .. } => {
						if s.is_none() {
							s = Some(values.is_empty())
						}
					}
					ControlFlow::If { condition, .. } => {
						if s.is_none() {
							s = Some(!*condition)
						}
					}
					ControlFlow::Capture { .. } => {
						ci = cf_stack.len() - index - 1
					}
				}
			}
			(s.unwrap_or(false), ci)
		};

		let mut output_buf = if capture_index == usize::max_value() {
			// TODO: Optimize
			// Reset capture_buf when we no longer use it.
			capture_buf = BufWriter::new(Vec::new());
			&mut parent_output_buf
		} else {
			&mut capture_buf
		};

		match state {
			State::RegularContent => match c {
				Char::OpenCurly => state = State::LastOpenBracket,
				Char::Newline | Char::Whitespace => {
					if !skipping {
						buffered_whitespace.push(byte)
					}
				}
				Char::CloseCurly | Char::Percent | Char::Dash | Char::Quote | Char::Other(..) => {
					if !skipping {
						if removing_whitespace {
							buffered_whitespace.clear()
						} else {
							flush_whitespace(&mut buffered_whitespace, output_buf)
						}
						removing_whitespace = false;
						write_to_stream(&[byte], output_buf)
					}
				}
			},
			State::LastOpenBracket => match c {
				Char::OpenCurly => state = State::ValueWaitingForDashOrWhitespace,
				Char::Percent => state = State::TagWaitingForDashOrWhitespace,
				Char::CloseCurly
				| Char::Newline
				| Char::Whitespace
				| Char::Quote
				| Char::Dash
				| Char::Other(..) => {
					if !skipping {
						write_to_stream(&[b'{', byte], output_buf)
					}
					state = State::RegularContent;
				}
			},
			State::ValueWaitingForDashOrWhitespace => match c {
				Char::Dash => {
					removing_whitespace = true;
					state = State::ValueNextIdentifier
				}
				Char::Whitespace | Char::Newline => {
					if removing_whitespace {
						buffered_whitespace.clear();
						removing_whitespace = false
					} else {
						flush_whitespace(&mut buffered_whitespace, output_buf)
					}
					state = State::ValueNextIdentifier
				}
				Char::OpenCurly | Char::CloseCurly | Char::Quote | Char::Percent | Char::Other(..) => panic_at_location(
					&format!("Unexpected non-whitespace, non-dash character \"{}\"",
						byte as char
					),
					&position,
					context,
				)
			}
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
							&outer_variables,
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
				Char::Dash => {
					current_identifier.push(byte);
					if removing_whitespace {
						state = State::ValueEncounteredDashInIdentifierOrEnd
					} else {
						state = State::ValueInIdentifier
					}
				}
				Char::Percent | Char::Other(..) => {
					current_identifier.push(byte);
					state = State::ValueInIdentifier
				}
			},
			State::ValueEncounteredDashInIdentifierOrEnd => match c {
				Char::CloseCurly => {
					current_identifier.pop();
					assert!(current_identifier.is_empty());

					if !skipping {
						output_template_value(
							&mut output_buf,
							&queued_identifiers,
							&outer_variables,
							&cf_stack,
							context,
						)
					}
					assert!(current_identifier.is_empty());
					queued_identifiers.clear();

					state = State::WaitingForCloseBracket
				}
				Char::Dash | Char::Quote | Char::Percent | Char::Newline | Char::Whitespace | Char::OpenCurly => panic_at_location(
					&format!("Unexpected character following dash: \"{}\"",
						byte as char
					),
					&position,
					context,
				),
				Char::Other(..) => {
					current_identifier.push(byte);
					state = State::ValueInIdentifier
				}
			}
			State::ValueInIdentifier => if parsing_literal {
				if current_identifier.len() > 1 && current_identifier.last() == Some(&b'"') {
					match c {
						Char::Whitespace | Char::Newline => {
							queued_identifiers.push(String::from_utf8_lossy(&current_identifier).to_string());
							current_identifier.clear();
							parsing_literal = false;
							state = State::ValueNextIdentifier
						},
						Char::Percent | Char::OpenCurly | Char::CloseCurly | Char::Quote | Char::Dash | Char::Other(..) => {
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
									&outer_variables,
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
					Char::OpenCurly | Char::Percent | Char::Dash | Char::Other(..) => {
						current_identifier.push(byte)
					}
				}
			},
			State::ValueEnd => match c {
				Char::CloseCurly => {
					if removing_whitespace {
						panic_at_location("Got beginning dash without ending dash before close-curly.", &position, context)
					}
					state = State::WaitingForCloseBracket
				}
				Char::Dash => {
					if !removing_whitespace {
						panic_at_location("Got ending dash without beginning dash.", &position, context)
					}
					state = State::ValueWaitingForFirstCloseBracket
				}
				Char::Whitespace | Char::Newline => {}
				Char::OpenCurly | Char::Percent | Char::Quote | Char::Other(..) => panic_at_location(
					&format!("Unexpected non-whitespace character \"{}\" when looking for value end curly braces.",
						byte as char
					),
					&position,
					context,
				)
			},
			State::ValueWaitingForFirstCloseBracket => match c {
				Char::CloseCurly => state = State::WaitingForCloseBracket,
				Char::Whitespace | Char::Newline | Char::OpenCurly | Char::Percent | Char::Quote | Char::Dash | Char::Other(..) => panic_at_location(
					&format!("Unexpected character \"{}\" following dash when expecting end curly braces.",
						byte as char
					),
					&position,
					context,
				)
			}
			State::TagWaitingForDashOrWhitespace => match c {
				Char::Dash => {
					removing_whitespace = true;
					state = State::TagStart
				}
				Char::Newline | Char::Whitespace => {
					if removing_whitespace {
						buffered_whitespace.clear();
						removing_whitespace = false
					} else {
						flush_whitespace(&mut buffered_whitespace, output_buf)
					}
					state = State::TagStart
				}
				Char::OpenCurly | Char::CloseCurly | Char::Percent | Char::Quote | Char::Other(..) => panic_at_location(
					&format!("Unexpected character \"{}\" following open tag looking for dash or whitespace.",
						byte as char
					),
					&position,
					context,
				)
			}
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
				Char::Dash | Char::Other(..) => {
					current_identifier.push(byte);
					state = State::TagFunction;
				}
			},
			State::TagFunction => match c {
				Char::Percent | Char::OpenCurly | Char::CloseCurly | Char::Dash => panic_at_location(
					&format!(
						"Unexpected character \"{}\" in function name.",
						byte as char
					),
					&position,
					context,
				),
				Char::Newline => {
					assert!(!current_identifier.is_empty());
					assert_eq!(queued_identifiers.len(), 0);
					queued_identifiers.push(String::from_utf8_lossy(&current_identifier).to_string());

					run_function(
						input_file,
						&mut output_buf,
						&queued_identifiers,
						&mut outer_variables,
						&mut cf_stack,
						skipping,
						context,
					);

					current_identifier.clear();
					assert!(!parsing_literal);
					queued_identifiers.clear();

					state = State::TagEnd
				},
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
				Char::Newline => {
					assert!(current_identifier.is_empty());

					run_function(
						input_file,
						&mut output_buf,
						&queued_identifiers,
						&mut outer_variables,
						&mut cf_stack,
						skipping,
						context,
					);

					current_identifier.clear();
					assert!(!parsing_literal);
					queued_identifiers.clear();

					state = State::TagEnd
				}
				Char::Percent => {
					if removing_whitespace {
						panic_at_location(
							"Got beginning dash without ending dash before percent.",
							&position,
							context,
						)
					}

					assert!(current_identifier.is_empty());

					let before_function_pos = input_file.seek(SeekFrom::Current(0)).unwrap_or_else(|e|
						panic!("Failed querying current stream position: {}", e));

					run_function(
						input_file,
						&mut output_buf,
						&queued_identifiers,
						&mut outer_variables,
						&mut cf_stack,
						skipping,
						context,
					);

					current_identifier.clear();
					assert!(!parsing_literal);
					queued_identifiers.clear();

					let end_function_pos = input_file.seek(SeekFrom::Current(0)).unwrap_or_else(|e|
						panic!("Failed querying current stream position: {}", e));
					state = if before_function_pos == end_function_pos {
						State::WaitingForCloseBracket
					} else {
						// Running the function may have caused us to seek back
						// in the stream to a prior tag (from after an endfor to
						// after a for).
						State::TagEnd
					};
				}
				Char::Quote => {
					current_identifier.push(byte);
					parsing_literal = true;
					state = State::TagInParameter
				}
				Char::Dash => {
					current_identifier.push(byte);
					if removing_whitespace {
						state = State::TagEncounteredDashInParameterOrEnd
					} else {
						state = State::TagInParameter
					}
				}
				Char::Other(..) => {
					current_identifier.push(byte);
					state = State::TagInParameter
				}
			},
			State::TagEncounteredDashInParameterOrEnd => match c {
				Char::Percent => {
					current_identifier.pop();
					assert!(current_identifier.is_empty());

					let before_function_pos = input_file.seek(SeekFrom::Current(0)).unwrap_or_else(|e|
						panic!("Failed querying current stream position: {}", e));

					run_function(
						input_file,
						&mut output_buf,
						&queued_identifiers,
						&mut outer_variables,
						&mut cf_stack,
						skipping,
						context,
					);

					assert!(current_identifier.is_empty());
					queued_identifiers.clear();

					let end_function_pos = input_file.seek(SeekFrom::Current(0)).unwrap_or_else(|e|
						panic!("Failed querying current stream position: {}", e));
					state = if before_function_pos == end_function_pos {
						State::WaitingForCloseBracket
					} else {
						// Running the function may have caused us to seek back
						// in the stream to a prior tag (from after an endfor to
						// after a for).
						State::TagEnd
					};
				}
				Char::Dash | Char::Quote | Char::CloseCurly | Char::Newline | Char::Whitespace | Char::OpenCurly => panic_at_location(
					&format!("Unexpected character following dash: \"{}\"",
						byte as char
					),
					&position,
					context,
				),
				Char::Other(..) => {
					current_identifier.push(byte);
					state = State::TagInParameter
				}
			}
			State::TagInParameter => if parsing_literal {
				if current_identifier.len() > 1 && current_identifier.last() == Some(&b'"') {
					match c {
						Char::Whitespace | Char::Newline => {
							queued_identifiers.push(String::from_utf8_lossy(&current_identifier).to_string());
							current_identifier.clear();
							parsing_literal = false;
							state = State::TagNextParameter
						},
						Char::Percent | Char::OpenCurly | Char::CloseCurly | Char::Quote | Char::Dash | Char::Other(..) => {
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

						run_function(
							input_file,
							&mut output_buf,
							&queued_identifiers,
							&mut outer_variables,
							&mut cf_stack,
							skipping,
							context,
						);

						current_identifier.clear();
						queued_identifiers.clear();

						state = State::TagEnd
					}
					Char::Quote => panic_at_location(
						"Unexpected quote (\") in the middle of non-literal.",
						&position,
						context,
					),
					Char::OpenCurly
					| Char::CloseCurly
					| Char::Percent
					| Char::Dash
					| Char::Other(..) => current_identifier.push(byte),
				}
			},
			State::TagEnd => match c {
				Char::Dash => {
					if !removing_whitespace {
						panic_at_location("Got ending dash without beginning dash.", &position, context)
					}
					state = State::WaitingForPercent
				}
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
			State::WaitingForPercent => match c {
				Char::Percent => state = State::WaitingForCloseBracket,
				Char::OpenCurly | Char::CloseCurly | Char::Newline | Char::Whitespace | Char::Quote | Char::Dash | Char::Other(..) => {
					panic_at_location(
						&format!(
							"Unexpected character \"{}\" when looking for ending percent.",
							byte as char
						),
						&position,
						context,
					)
				}
			}
			State::WaitingForCloseBracket => match c {
				Char::CloseCurly => state = State::RegularContent,
				Char::OpenCurly
				| Char::Percent
				| Char::Newline
				| Char::Whitespace
				| Char::Quote
				| Char::Dash
				| Char::Other(..) => panic_at_location(
					&format!(
							"Unexpected character \"{}\" when looking for close-bracket.",
							byte as char
					),
					&position,
					context,
				),
			},
		}

		// Down here we are free to mutate cf_stack again without running into
		// shared reference errors.
		if capture_index < cf_stack.len() {
			match &mut cf_stack[capture_index] {
				ControlFlow::Capture { content, .. } => {
					if !capture_buf.buffer().is_empty()
						|| !capture_buf.get_ref().is_empty()
					{
						content.append(
							&mut capture_buf.into_inner().unwrap_or_else(|e| {
								panic!(
									"Failed unwrapping capture BufWriter: {}",
									e
								)
							}),
						);
						capture_buf = BufWriter::new(Vec::new())
					}
				}
				_ => panic!(
					"Expected capture at stack index {} but got {:?}.",
					capture_index, cf_stack[capture_index]
				),
			}
		}

		// This does not account for back-seeks due to for loops.
		match c {
			Char::Newline => {
				position.line += 1;
				position.column = 1;
			}
			_ => position.column += 1,
		}
	}

	match state {
		State::RegularContent => {
			flush_whitespace(&mut buffered_whitespace, parent_output_buf)
		}
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
	outer_variables: &HashMap<String, Value>,
	cf_stack: &[ControlFlow],
	context: &Context,
) {
	const EXAMPLE_DATETIME: &str = "2001-01-19T20:10:01Z";

	if identifiers.is_empty() {
		panic!("Encountered empty template value section, missing name.")
	}
	let name = &identifiers[0];
	let mut value =
		match fetch_template_value(name, outer_variables, cf_stack, context) {
			Value::Boolean(b) => b.to_string(),
			Value::String(s) => s,
			Value::Integer(i) => i.to_string(),
			Value::List { .. } => panic!(
				"Cannot output list value {} directly, maybe use a for-loop?",
				name
			),
			Value::Dictionary { .. } => {
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
			"append" => {
				if identifiers.len() < offset + 2 {
					panic!("append filter function requires a parameter.")
				}
				value.push_str(
					&fetch_template_value(
						&identifiers[offset + 2],
						outer_variables,
						cf_stack,
						context,
					)
					.string_content(),
				);
				offset += 3
			}
			"date" => {
				if identifiers.len() < offset + 2 {
					panic!("date filter function requires a parameter.")
				}
				let format_string = match fetch_template_value(
					&identifiers[offset + 2],
					outer_variables,
					cf_stack,
					context,
				) {
					Value::String(s) => s,
					_ => panic!(
						"Cannot handle non-string value as format string."
					),
				};

				if value.len() != EXAMPLE_DATETIME.len() {
					panic!("date filter requires valid date format such as \"{}\", but got: \"{}\"", EXAMPLE_DATETIME, value)
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
	outer_variables: &HashMap<String, Value>,
	cf_stack: &[ControlFlow],
	context: &Context,
) -> Value {
	assert!(!name.is_empty(), "Never expected to get empty identifiers.");

	if name.len() > 1 && name.starts_with('"') && name.ends_with('"') {
		return Value::String(name[1..name.len() - 1].to_string());
	}

	if name == "true" || name == "false" {
		return Value::Boolean(name.parse::<bool>().unwrap_or_else(|e| {
			panic!("Failed converting {} to boolean: {}", name, e)
		}));
	}

	{
		let numeric_offset = if name.starts_with('-') { 1 } else { 0 };
		if name[numeric_offset..].chars().all(|c| c.is_digit(10)) {
			return Value::Integer(name.parse::<i32>().unwrap_or_else(|e| {
				panic!("Failed converting {} to an i32: {}", name, e)
			}));
		}
	}

	let name_parts: Vec<&str> = name.split('.').collect();
	match name_parts.len() {
		1 => fetch_value(name, outer_variables, cf_stack, context),
		2 => fetch_field(
			name_parts[0],
			name_parts[1],
			outer_variables,
			cf_stack,
			context,
		),
		3 => {
			let value = fetch_field(
				name_parts[0],
				name_parts[1],
				outer_variables,
				cf_stack,
				context,
			);

			value.get_field(name_parts[2])
		}
		_ => panic!("Unexpected identifier: {}", name),
	}
}

fn fetch_field(
	object: &str,
	field: &str,
	outer_variables: &HashMap<String, Value>,
	cf_stack: &[ControlFlow],
	context: &Context,
) -> Value {
	if object.is_empty() {
		panic!("Empty object name.")
	}
	if field.is_empty() {
		panic!("Empty field name.")
	}

	match object {
		"page" => match field {
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
			"published" => Value::Boolean(context.front_matter.published),
			"edited" => Value::String(
				context
					.front_matter
					.edited
					.as_ref()
					.map_or_else(String::new, String::clone),
			),
			"categories" => Value::List {
				values: context
					.front_matter
					.categories
					.iter()
					.map(|tag| Value::String(tag.clone()))
					.collect(),
			},
			"tags" => Value::List {
				values: context
					.front_matter
					.tags
					.iter()
					.map(|tag| Value::String(tag.clone()))
					.collect(),
			},
			_ => {
				if let Some(value) =
					context.front_matter.custom_attributes.get(field)
				{
					Value::String(value.to_string())
				} else {
					panic!("Not yet supported field: {}.{}", object, field)
				}
			}
		},
		"site" => match field {
			"title" => Value::String(context.site_info.title.to_string()),
			_ => panic!(
				"Trying to access unsupported field: {}.{}",
				object, field
			),
		},
		"forloop" => match field {
			"first" => {
				for cf in cf_stack.iter().rev() {
					if let ControlFlow::For { index, .. } = cf {
						return Value::Boolean(*index == 0);
					}
				}
				panic!("Trying to access forloop property {} while not in for-loop.", field)
			}
			_ => panic!("Unsupported forloop property: {}", field),
		},
		_ => {
			let mut value = None;
			for cf in cf_stack.iter().rev() {
				match cf {
					ControlFlow::For {
						local_variables, ..
					}
					| ControlFlow::If {
						local_variables, ..
					} => {
						value = local_variables.get(object);
						if value.is_some() {
							break;
						}
					}
					ControlFlow::Capture { .. } => {}
				}
			}

			if value.is_none() {
				value = outer_variables.get(object)
			}

			if let Some(value) = value {
				return value.get_field(field);
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
					_ => panic!(
						"Unhandled field {} on object {}.",
						field, object
					),
				}
			}

			panic!(
				"Unhandled object \"{}\", file: {}",
				object,
				context.input_file_path.display()
			)
		}
	}
}

fn fetch_value(
	name: &str,
	outer_variables: &HashMap<String, Value>,
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
			let mut link = String::from("/");
			link.push_str(
				&strip_prefix(&entry.path, context.root_input_dir)
					.to_string_lossy(),
			);
			map.insert("link", Value::String(link));
			map.insert(
				"published",
				Value::Boolean(entry.front_matter.published),
			);

			result.push(Value::Dictionary { map })
		}
		return Value::List { values: result };
	}

	for cf in cf_stack.iter().rev() {
		match cf {
			ControlFlow::For {
				local_variables, ..
			}
			| ControlFlow::If {
				local_variables, ..
			} => {
				if let Some(value) = local_variables.get(name) {
					return value.clone();
				}
			}
			ControlFlow::Capture { .. } => {}
		}
	}

	if let Some(value) = outer_variables.get(name) {
		return value.clone();
	}

	panic!("Failed finding value for \"{}\"", name);
}

fn run_function<T: Read + Seek>(
	input_file: &mut BufReader<T>,
	mut output_buf: &mut BufWriter<Vec<u8>>,
	identifiers: &[String],
	outer_variables: &mut HashMap<String, Value>,
	cf_stack: &mut Vec<ControlFlow>,
	skipping: bool,
	context: &Context,
) {
	let function = identifiers[0].as_str();
	let parameters = &identifiers[1..];

	if function.is_empty() {
		panic!("Empty function name.")
	}

	match function {
		"assign" => {
			assign(parameters, outer_variables, cf_stack, skipping, context)
		}
		"capture" => start_capture(parameters, cf_stack, skipping),
		"endcapture" => {
			end_capture(parameters, outer_variables, cf_stack, skipping)
		}
		"if" => {
			start_if(parameters, outer_variables, cf_stack, skipping, context)
		}
		"else" => r#else(parameters, cf_stack),
		"endif" => end_if(parameters, cf_stack),
		"for" => start_for(
			input_file,
			parameters,
			outer_variables,
			cf_stack,
			skipping,
			context,
		),
		"endfor" => end_for(input_file, parameters, cf_stack),
		"include" => include_file(
			&mut output_buf,
			parameters,
			outer_variables,
			cf_stack,
			skipping,
			context,
		),
		"link" => check_and_emit_link(
			context.output_file_path,
			&mut output_buf,
			parameters,
			outer_variables,
			cf_stack,
			skipping,
			context,
		),
		_ => panic!("Unsupported function: {}", function),
	}
}

fn assign(
	parameters: &[String],
	outer_variables: &mut HashMap<String, Value>,
	cf_stack: &mut Vec<ControlFlow>,
	skipping: bool,
	context: &Context,
) {
	if skipping {
		return;
	}
	if parameters.len() != 3 {
		panic!("assign-statement doesn't have the correct parameter count, expecting \"assign .. = ..\", got: {:?}", parameters)
	}
	if parameters[1] != "=" {
		panic!(
			"Missing assignment-operator (=) in assign-statement, got: {}",
			parameters[1]
		)
	}

	let name = &parameters[0];
	let value = fetch_template_value(
		&parameters[2],
		outer_variables,
		cf_stack,
		context,
	);

	assign_inner(name, value, outer_variables, cf_stack)
}

fn assign_inner(
	name: &str,
	value: Value,
	outer_variables: &mut HashMap<String, Value>,
	cf_stack: &mut Vec<ControlFlow>,
) {
	if !cf_stack.is_empty() {
		for cf in cf_stack.iter_mut().rev() {
			match cf {
				ControlFlow::For {
					local_variables, ..
				}
				| ControlFlow::If {
					local_variables, ..
				} => {
					if let Some(v) = local_variables.get_mut(name) {
						*v = value;

						return;
					}
				}
				ControlFlow::Capture { .. } => {}
			};
		}

		if let Some(v) = outer_variables.get_mut(name) {
			*v = value;

			return;
		}

		for cf in cf_stack.iter_mut().rev() {
			match cf {
				ControlFlow::For {
					local_variables, ..
				}
				| ControlFlow::If {
					local_variables, ..
				} => {
					local_variables.insert(name.to_string(), value);

					return;
				}
				ControlFlow::Capture { .. } => {}
			}
		}
	}

	outer_variables.insert(name.to_string(), value);
}

fn start_capture(
	parameters: &[String],
	cf_stack: &mut Vec<ControlFlow>,
	skipping: bool,
) {
	if skipping {
		return;
	}
	if parameters.len() != 1 {
		panic!("capture-statement doesn't have the correct parameter count, expecting \"capture ..\", got: {:?}", parameters)
	}

	cf_stack.push(ControlFlow::Capture {
		variable: parameters[0].clone(),
		content: Vec::new(),
	})
}

fn end_capture(
	parameters: &[String],
	outer_variables: &mut HashMap<String, Value>,
	cf_stack: &mut Vec<ControlFlow>,
	skipping: bool,
) {
	if skipping {
		return;
	}
	if !parameters.is_empty() {
		panic!(
			"Expecting no parameters to endcapture. Encountered: {:?}",
			parameters
		)
	}

	let cf = cf_stack
		.pop()
		.expect("Encountered endcapture when control flow stack was empty.");
	match cf {
		ControlFlow::Capture { variable, content } => assign_inner(
			&variable,
			Value::String(String::from_utf8_lossy(&content).to_string()),
			outer_variables,
			cf_stack,
		),
		_ => panic!(
			"Expected capture as front-most control flow element but got: {:?}",
			cf
		),
	}
}

fn start_if(
	parameters: &[String],
	outer_variables: &mut HashMap<String, Value>,
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

	let lhs = fetch_template_value(
		&parameters[0],
		outer_variables,
		cf_stack,
		context,
	);
	let rhs = fetch_template_value(
		&parameters[2],
		outer_variables,
		cf_stack,
		context,
	);

	println!("{:?} => {:?} {} {:?}", parameters, lhs, parameters[1], rhs);
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

fn r#else(parameters: &[String], cf_stack: &mut Vec<ControlFlow>) {
	if !parameters.is_empty() {
		panic!(
			"Expecting no parameters to else. Encountered: {:?}",
			parameters
		)
	}

	let cf = cf_stack
		.last_mut()
		.expect("Encountered else when control flow stack was empty.");
	match cf {
		ControlFlow::If {
			condition,
			local_variables,
		} => {
			local_variables.clear();
			*condition = !*condition
		}
		_ => panic!(
			"Encountered else without match preceeding if, had {:?} instead.",
			cf
		),
	}
}

fn end_if(parameters: &[String], cf_stack: &mut Vec<ControlFlow>) {
	if !parameters.is_empty() {
		panic!(
			"Expecting no parameters to endif. Encountered: {:?}",
			parameters
		)
	}

	let cf = cf_stack
		.pop()
		.expect("Encountered endif when control flow stack was empty.");
	match cf {
		ControlFlow::If { .. } => {}
		_ => panic!(
			"Encountered endif without match preceeding if, had {:?} instead.",
			cf
		),
	}
}

fn start_for<T: Read + Seek>(
	input_file: &mut BufReader<T>,
	parameters: &[String],
	outer_variables: &mut HashMap<String, Value>,
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
		let value = fetch_template_value(
			loop_values_name,
			outer_variables,
			cf_stack,
			context,
		);
		match value {
			Value::String(s) => {
				s.chars().map(|c| Value::String(c.to_string())).collect()
			}
			Value::Boolean(..) | Value::Integer(..) => {
				panic!("Cannot iterate over one single {:?}.", value)
			}
			Value::List { values } => values,
			Value::Dictionary { map } => map.values().cloned().collect(),
		}
	};

	let limit = if parameters.len() > 3 {
		if parameters.len() != 4 {
			panic!("Expected 3-4 parameters to for loop of the form \"for .. in .. limit ..\", but got {} parameters.", parameters.len());
		}

		let limit_str = if parameters[3].starts_with("limit:") {
			&parameters[3][6..]
		} else {
			panic!(
				"Expected 4th parameter to for loop to be \"limit:...\" but got {}",
				parameters[3]
			);
		};

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

	let mut local_variables = HashMap::new();
	if end > 0 {
		local_variables.insert(variable.clone(), loop_values[0].clone());
	}

	// Store position to seek back to as right before (-1) the character that
	// started the ending of the current for-tag so that it gets re-read when
	// seeking back, making the state machine function properly.
	let buffer_start_position =
		input_file.seek(SeekFrom::Current(0)).unwrap_or_else(|e| {
			panic!("Failed fetching current position from input stream: {}", e)
		}) - 1;

	cf_stack.push(ControlFlow::For {
		end,
		local_variables,
		values: loop_values,
		variable: variable.clone(),
		index: 0,
		buffer_start_position,
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
			variable,
			values,
			index,
			end,
			buffer_start_position,
			local_variables,
			..
		} => {
			*index += 1;
			local_variables.clear();
			if index < end {
				local_variables
					.insert(variable.clone(), values[*index].clone());

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
	parameters: &[String],
	outer_variables: &mut HashMap<String, Value>,
	cf_stack: &mut Vec<ControlFlow>,
	skipping: bool,
	context: &Context,
) {
	if skipping {
		return;
	}

	if parameters.len() != 1 {
		panic!("Expecting only 1 parameter for include operation. Encountered: {:?}", parameters)
	}

	let included_file_path = context
		.root_input_dir
		.join("_includes")
		.join(&parameters[0]);

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

	let mut outer_variables_for_include = outer_variables.clone();
	for cf in cf_stack {
		match cf {
			ControlFlow::For {
				local_variables, ..
			} => {
				for (name, value) in local_variables {
					outer_variables_for_include
						.insert(name.clone(), value.clone());
				}
			}
			ControlFlow::If { .. } | ControlFlow::Capture { .. } => {}
		}
	}

	process(
		&mut included_file,
		&mut output_buf,
		outer_variables_for_include,
		&Context {
			input_file_path: &included_file_path,
			..*context
		},
	)
}

fn check_and_emit_link(
	output_file_path: &PathBuf,
	output_buf: &mut BufWriter<Vec<u8>>,
	parameters: &[String],
	outer_variables: &HashMap<String, Value>,
	cf_stack: &[ControlFlow],
	skipping: bool,
	context: &Context,
) {
	if skipping {
		return;
	}

	if parameters.len() != 1 {
		panic!(
			"Expecting 1 parameter in link operation. Encountered: {:?}",
			parameters
		)
	}

	let value = fetch_template_value(
		&parameters[0],
		outer_variables,
		cf_stack,
		context,
	);
	let parameter = match value {
		Value::String(s) => s,
		_ => panic!(
			"Expected string value but got {} - {:?}",
			parameters[0], value
		),
	};

	let append_index_html = parameter.ends_with('/');
	if !parameter.starts_with('/') {
		panic!(
			"Only absolute paths are allowed in links, but got: {} (file: {})",
			parameter,
			context.input_file_path.display()
		);
	}
	let mut path = context.root_input_dir.join(PathBuf::from(&parameter[1..]));
	if append_index_html {
		path = path.join(PathBuf::from("index.html"));
	}

	let linked_output_path = &match context.input_output_map.get(&path) {
		Some(lo) => lo,
		_ => panic!(
			"Failed finding {} among: {:#?}",
			path.display(),
			context.input_output_map.keys()
		),
	}
	.file
	.path;

	write_to_stream(
		make_relative_link(
			output_file_path,
			linked_output_path,
			context.root_output_dir,
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

	if linked_output_path_stripped.file_name() == Some(OsStr::new("index.html"))
	{
		linked_output_path_stripped =
			linked_output_path_stripped.with_file_name("")
	}

	println!(
		"File: {}, translated link: {}, prefix+slash: {}, result: {}",
		output_file_path.display(),
		linked_output_path.display(),
		prefix_plus_slash,
		linked_output_path_stripped.display()
	);

	linked_output_path_stripped.to_string_lossy().to_string()
}
