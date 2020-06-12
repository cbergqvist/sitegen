use std::borrow::Borrow;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{
	BufRead, BufReader, BufWriter, ErrorKind, Read, Seek, SeekFrom, Write,
};
use std::net::{TcpListener, TcpStream};
use std::option::Option;
use std::path::PathBuf;
use std::string::String;
use std::sync::mpsc::channel;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use std::{env, fmt, fs};

use pulldown_cmark::{html, Parser};

use notify::{watcher, RecursiveMode, Watcher};

use yaml_rust::YamlLoader;

struct FrontMatter {
	title: String,
	date: String,
	published: bool,
	edited: Option<String>,
	categories: Vec<String>,
	tags: Vec<String>,
	layout: Option<String>,
	custom_attributes: BTreeMap<String, String>,
}

struct BoolArg {
	name: &'static str,
	help: &'static str,
	value: bool,
}

struct I16Arg {
	name: &'static str,
	help: &'static str,
	value: i16,
	set: bool,
}

struct StringArg {
	name: &'static str,
	help: &'static str,
	value: String,
	set: bool,
}

impl fmt::Display for BoolArg {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "-{} {}", self.name, self.help)
	}
}

impl fmt::Display for I16Arg {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "-{} {}", self.name, self.help)
	}
}

impl fmt::Display for StringArg {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "-{} {}", self.name, self.help)
	}
}

struct Refresh {
	index: u32,
	file: Option<PathBuf>,
}

enum ReadResult {
	GetRequest(PathBuf),
	WebSocket(String),
}

fn main() {
	// Not using the otherwise brilliant CLAP crate since I detest string matching args to get their values.
	let mut help_arg = BoolArg {
		name: "help",
		help: "Print this text.",
		value: false,
	};
	let mut host_arg = StringArg {
		name: "host",
		help: "Set address to bind to. The default 127.0.0.1 can be used for privacy and 0.0.0.0 to give access to other machines.",
		value: String::from("127.0.0.1"),
		set: false,
	};
	let mut input_arg = StringArg {
		name: "input",
		help: "Set input directory to process.",
		value: String::from("./input"),
		set: false,
	};
	let mut output_arg = StringArg {
		name: "output",
		help: "Set output directory to write to.",
		value: String::from("./output"),
		set: false,
	};
	let mut port_arg = I16Arg {
		name: "port",
		help: "Set port to bind to.",
		value: 8090,
		set: false,
	};
	let mut watch_arg = BoolArg {
		name: "watch",
		help: "Run indefinitely, watching input directory for changes.",
		value: false,
	};

	parse_args(
		&mut vec![&mut help_arg, &mut watch_arg],
		&mut vec![&mut port_arg],
		&mut vec![&mut host_arg, &mut input_arg, &mut output_arg],
	);

	if help_arg.value {
		println!(
			"SiteGen version 0.1
Christopher Bergqvist <chris@digitalpoetry.se>

Basic static site generator.

Arguments:"
		);
		println!("{}", help_arg);
		println!("{}", host_arg);
		println!("{}", input_arg);
		println!("{}", output_arg);
		println!("{}", port_arg);
		println!("{}", watch_arg);

		return;
	}

	if !watch_arg.value && (host_arg.set || port_arg.set) {
		println!(
			"WARNING: {} or {} arg set without {} arg, so they have no use.",
			host_arg.name, port_arg.name, watch_arg.name
		)
	}

	inner_main(
		&PathBuf::from(input_arg.value),
		&PathBuf::from(output_arg.value),
		&host_arg.value,
		port_arg.value,
		watch_arg.value,
	)
}

fn inner_main(
	input_dir: &PathBuf,
	output_dir: &PathBuf,
	host: &str,
	port: i16,
	watch: bool,
) {
	let markdown_extension = OsStr::new("md");

	let mut output_files = Vec::new();

	let markdown_files = get_markdown_files(&input_dir, markdown_extension);

	if markdown_files.is_empty() {
		println!(
			"Found no valid file entries under \"{}\".",
			input_dir.display()
		);
	} else {
		fs::create_dir(&output_dir).unwrap_or_else(|e| {
			panic!("Failed creating \"{}\": {}.", output_dir.display(), e)
		});

		for file_name in &markdown_files {
			output_files.push(process_markdown_file(
				&file_name,
				&input_dir,
				&output_dir,
			))
		}
	}

	if !watch {
		return;
	}

	let fs_cond = Arc::new((
		Mutex::new(Refresh {
			index: 0,
			file: None,
		}),
		Condvar::new(),
	));

	let root_dir = PathBuf::from(&output_dir);
	let fs_cond_clone = fs_cond.clone();
	let start_file = output_files.first().cloned();

	let listening_thread =
		spawn_listening_thread(host, port, root_dir, fs_cond, start_file);

	// As we start watching some time after we've done initial processing, it is
	// possible that files get modified in between and changes get lost.
	watch_fs(&input_dir, &output_dir, markdown_extension, &fs_cond_clone);

	// We never really get here as we loop infinitely until Ctrl+C.
	listening_thread
		.join()
		.expect("Failed joining listening thread.");
}

fn spawn_listening_thread(
	host: &str,
	port: i16,
	root_dir: PathBuf,
	fs_cond: Arc<(Mutex<Refresh>, Condvar)>,
	start_file: Option<PathBuf>,
) -> thread::JoinHandle<()> {
	let listener = TcpListener::bind(format!("{}:{}", host, port))
		.unwrap_or_else(|e| {
			panic!("Failed to bind TCP listening port {}:{}: {}", host, port, e)
		});
	println!("Listening for connections on {}:{}", host, port);

	thread::spawn(move || {
		for stream in listener.incoming() {
			match stream {
				Ok(stream) => {
					let root_dir_clone = root_dir.clone();
					let fs_cond_pair_clone = fs_cond.clone();
					let start_file_clone = start_file.clone();
					thread::spawn(move || {
						handle_client(
							stream,
							&root_dir_clone,
							&fs_cond_pair_clone,
							start_file_clone,
						)
					});
				}
				Err(e) => println!("WARNING: Unable to connect: {}", e),
			}
		}
	})
}

fn watch_fs(
	input_dir: &PathBuf,
	output_dir: &PathBuf,
	markdown_extension: &OsStr,
	fs_cond: &Arc<(Mutex<Refresh>, Condvar)>,
) {
	let (tx, rx) = channel();
	let mut watcher =
		watcher(tx, Duration::from_millis(200)).unwrap_or_else(|e| {
			panic!("Unable to create watcher: {}", e);
		});

	watcher
		.watch(&input_dir, RecursiveMode::Recursive)
		.unwrap_or_else(|e| {
			panic!("Unable to watch {}: {}", input_dir.display(), e);
		});

	let html_extension = OsStr::new("html");
	loop {
		match rx.recv() {
			Ok(event) => {
				println!("Got {:?}", event);
				match event {
					notify::DebouncedEvent::Write(path)
					| notify::DebouncedEvent::Create(path) => {
						let path_to_communicate = get_path_to_refresh(
							path,
							markdown_extension,
							html_extension,
							input_dir,
							output_dir,
						);
						if path_to_communicate.is_some() {
							let (mutex, cvar) = &**fs_cond;

							let mut refresh =
								mutex.lock().unwrap_or_else(|e| {
									panic!("Failed locking mutex: {}", e)
								});
							refresh.file = path_to_communicate;
							refresh.index += 1;
							cvar.notify_all();
						}
					}
					_ => {
						println!("Skipping event.");
					}
				}
			}
			Err(e) => panic!("Watch error: {}", e),
		}
	}
}

fn get_path_to_refresh(
	mut path: PathBuf,
	markdown_extension: &OsStr,
	html_extension: &OsStr,
	input_dir: &PathBuf,
	output_dir: &PathBuf,
) -> Option<PathBuf> {
	path = path.canonicalize().unwrap_or_else(|e| {
		panic!("Canonicalization of {} failed: {}", path.display(), e)
	});
	if path.extension() == Some(markdown_extension) {
		match fs::create_dir(&output_dir) {
			Ok(_) => {}
			Err(e) => {
				if e.kind() != ErrorKind::AlreadyExists {
					panic!(
						"Failed creating \"{}\": {}.",
						output_dir.display(),
						e
					)
				}
			}
		}

		return Some(process_markdown_file(&path, &input_dir, &output_dir));
	} else if path.extension() == Some(html_extension) {
		let parent_path = path.parent().unwrap_or_else(|| {
			panic!("Path without a parent directory?: {}", path.display())
		});
		let parent_path_file_name =
			parent_path.file_name().unwrap_or_else(|| {
				panic!("Missing file name in path: {}", parent_path.display())
			});
		if parent_path_file_name == "_templates" {
			let file_stem = path.file_stem().unwrap_or_else(|| {
				panic!("Missing file stem in path: {}", path.display())
			});
			let mut dir_name = OsString::from(file_stem);
			dir_name.push("s");
			let markdown_dir = input_dir.join(dir_name);
			let markdown_files = if markdown_dir.exists() {
				get_markdown_files(&markdown_dir, markdown_extension)
			} else {
				Vec::new()
			};

			if markdown_files.is_empty() {
				let templated_file = input_dir
					.join(file_stem)
					.with_extension(markdown_extension);
				if templated_file.exists() {
					return Some(process_markdown_file(
						&templated_file,
						&input_dir,
						&output_dir,
					));
				}
			} else {
				let mut output_files = Vec::new();
				for file_name in &markdown_files {
					output_files.push(process_markdown_file(
						&file_name,
						&input_dir,
						&output_dir,
					))
				}
				return output_files.first().cloned();
			}
		}
	}

	None
}
fn parse_args(
	bool_args: &mut Vec<&mut BoolArg>,
	i16_args: &mut Vec<&mut I16Arg>,
	string_args: &mut Vec<&mut StringArg>,
) {
	let mut first_arg = true;
	let mut previous_arg = None;
	'arg_loop: for mut arg in env::args() {
		// Skip executable arg itself.
		if first_arg {
			first_arg = false;
			continue;
		}

		if let Some(prev) = previous_arg {
			for string_arg in &mut *string_args {
				if prev == string_arg.name {
					string_arg.value = arg;
					string_arg.set = true;
					previous_arg = None;
					continue 'arg_loop;
				}
			}

			for i16_arg in &mut *i16_args {
				if prev == i16_arg.name {
					i16_arg.value = arg.parse::<i16>().unwrap_or_else(|e| {
						panic!("Invalid value for {}: {}", i16_arg.name, e);
					});
					i16_arg.set = true;
					previous_arg = None;
					continue 'arg_loop;
				}
			}

			panic!("Unhandled key-value arg: {}", prev);
		}

		if arg.len() < 3
			|| arg.as_bytes()[0] != b'-'
			|| arg.as_bytes()[1] != b'-'
		{
			panic!("Unexpected argument: {}", arg)
		}

		arg = arg.split_off(2);

		for bool_arg in &mut *bool_args {
			if arg == bool_arg.name {
				bool_arg.value = true;
				continue 'arg_loop;
			}
		}

		for i16_arg in &*i16_args {
			if arg == i16_arg.name {
				previous_arg = Some(arg);
				continue 'arg_loop;
			}
		}

		for string_arg in &*string_args {
			if arg == string_arg.name {
				previous_arg = Some(arg);
				continue 'arg_loop;
			}
		}

		panic!("Unsupported argument: {}", arg)
	}
}

fn get_markdown_files(
	input_dir: &PathBuf,
	markdown_extension: &OsStr,
) -> Vec<PathBuf> {
	let entries = fs::read_dir(input_dir).unwrap_or_else(|e| {
		panic!(
			"Failed reading paths from \"{}\": {}.",
			input_dir.display(),
			e
		)
	});
	let mut files = Vec::new();
	for entry in entries {
		match entry {
			Ok(entry) => {
				let path = entry.path();
				if let Ok(ft) = entry.file_type() {
					if ft.is_file() {
						if path.extension() == Some(markdown_extension) {
							files.push(path);
							println!(
								"Markdown!: \"{}\"",
								entry.path().display()
							);
						} else {
							println!(
								"Skipping non-.md file: \"{}\"",
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
						} else {
							let mut subdir_files =
								get_markdown_files(&path, markdown_extension);
							files.append(&mut subdir_files);
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

	files
}

fn process_markdown_file(
	input_file_path: &PathBuf,
	root_input_dir: &PathBuf,
	root_output_dir: &PathBuf,
) -> PathBuf {
	let timer = Instant::now();
	let input_file = fs::File::open(&input_file_path).unwrap_or_else(|e| {
		panic!("Failed opening \"{}\": {}.", &input_file_path.display(), e)
	});

	let mut reader = BufReader::new(input_file);

	let front_matter = parse_front_matter(&input_file_path, &mut reader);
	let mut markdown_content = String::new();
	let _size =
		reader
			.read_to_string(&mut markdown_content)
			.unwrap_or_else(|e| {
				panic!(
					"Failed reading Markdown content from \"{}\": {}.",
					&input_file_path.display(),
					e
				)
			});
	let parser = Parser::new(&markdown_content);
	let mut output = Vec::new();
	let mut output_buf = BufWriter::new(&mut output);

	write_html_page(
		&mut output_buf,
		&front_matter,
		parser,
		input_file_path,
		root_input_dir,
	);

	let output_file_path = compute_output_file_path(
		input_file_path,
		root_input_dir,
		root_output_dir,
	);

	let closest_output_dir = output_file_path.parent().unwrap_or_else(|| {
		panic!(
			"Output file path without a parent directory?: {}",
			output_file_path.display()
		)
	});
	fs::create_dir_all(closest_output_dir).unwrap_or_else(|e| {
		panic!(
			"Failed creating directories for {}: {}",
			closest_output_dir.display(),
			e
		)
	});

	let mut output_file =
		fs::File::create(&output_file_path).unwrap_or_else(|e| {
			panic!(
				"Failed creating \"{}\": {}.",
				&output_file_path.display(),
				e
			)
		});
	output_file
		.write_all(&output_buf.buffer())
		.unwrap_or_else(|e| {
			panic!(
				"Failed writing to \"{}\": {}.",
				&output_file_path.display(),
				e
			)
		});

	// Avoiding sync_all() for now to be friendlier to disks.
	output_file.sync_data().unwrap_or_else(|e| {
		panic!(
			"Failed sync_data() for \"{}\": {}.",
			&output_file_path.display(),
			e
		)
	});

	println!(
		"Converted {} to {} after {} ms.",
		input_file_path.display(),
		output_file_path.display(),
		timer.elapsed().as_millis()
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

fn compute_output_file_path(
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

fn write_html_page(
	mut output_buf: &mut BufWriter<&mut Vec<u8>>,
	front_matter: &FrontMatter,
	mut parser: Parser,
	input_file_path: &PathBuf,
	root_input_dir: &PathBuf,
) {
	enum State {
		JustHtml,
		LastOpenBracket,
		Template,
		TemplateObject,
		TemplateField,
		TemplateTrailingWhitespace,
		FirstCloseBracket,
	}

	let template_file_path =
		compute_template_file_path(input_file_path, root_input_dir);
	let mut template_file =
		fs::File::open(&template_file_path).unwrap_or_else(|e| {
			panic!(
				"Failed opening template file {}: {}",
				template_file_path.display(),
				e
			)
		});
	let mut state = State::JustHtml;
	let mut buf = [0_u8; 64 * 1024];
	let mut line_number = 1;
	let mut column_number = 1;
	let mut object = Vec::new();
	let mut field = Vec::new();
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
					State::JustHtml => { write_to_output(output_buf, &[byte]); }
					State::LastOpenBracket => { write_to_output(output_buf, b"{\n"); state = State::JustHtml }
					State::TemplateObject => panic!("Unexpected newline while reading template identifier at line {}, column {}.", line_number, column_number),
					State::TemplateField => { output_template_value(&mut output_buf, &mut object, &mut field, &front_matter, &mut parser, input_file_path) }
					State::FirstCloseBracket => panic!("Expected close bracket but got newline at line {}, column {}.", line_number, column_number),
					State::Template | State::TemplateTrailingWhitespace => {}
				}
				line_number += 1;
				column_number = 1;
			} else {
				match state {
					State::JustHtml => {
						match byte {
							b'{' => state = State::LastOpenBracket,
							_ => write_to_output(output_buf, &[byte])
						}
					},
					State::LastOpenBracket => {
						if byte == b'{' {
							state = State::Template
						} else {
							write_to_output(output_buf, &[b'{']);
							state = State::JustHtml;
						}
					},
					State::Template => match byte {
						b'{' => panic!("Unexpected open bracket while in template mode at line {}, column {}.", line_number, column_number),
						b' ' | b'\t' => {},
						_ => {
							object.push(byte);
							state = State::TemplateObject;
						}
					},
					State::TemplateObject => {
						if byte == b'.' {
							state = State::TemplateField;
						} else {
							object.push(byte);
						}
					},
					State::TemplateField => {
						match byte {
							b'.' => panic!("Additional dot in template identifier at line {}, column {}.", line_number, column_number),
							b'}' => state = State::FirstCloseBracket,
							b' ' | b'\t' => {
								output_template_value(&mut output_buf, &mut object, &mut field, &front_matter, &mut parser, input_file_path);
								state = State::TemplateTrailingWhitespace
							}
							_ => field.push(byte)
						}
					},
					State::TemplateTrailingWhitespace => {
						match byte {
						b'}' => state = State::FirstCloseBracket,
						b' ' | b'\t' => {}
						_ => panic!("Unexpected non-whitespace character at line {}, column {}.", line_number, column_number)
						}
					}
					State::FirstCloseBracket => {
						if byte == b'}' {
							state = State::JustHtml;
						} else {
							panic!("Missing double close-bracket at line {}, column {}.", line_number, column_number)
						}
					}
				}
			}
		}
	}
}

fn compute_template_file_path(
	input_file_path: &PathBuf,
	root_input_dir: &PathBuf,
) -> PathBuf {
	let mut template_file_path = PathBuf::from(root_input_dir);
	template_file_path.push("_templates");
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
	if template_name.ends_with('s') {
		template_name.truncate(template_name.len() - 1)
	}
	template_file_path.push(template_name);
	template_file_path.set_extension("html");

	template_file_path
}

fn write_to_output(output_buf: &mut BufWriter<&mut Vec<u8>>, data: &[u8]) {
	output_buf.write_all(data).unwrap_or_else(|e| {
		panic!("Failed writing \"{:?}\" to to buffer: {}.", data, e)
	});
}

fn output_template_value(
	mut output_buf: &mut BufWriter<&mut Vec<u8>>,
	object: &mut Vec<u8>,
	field: &mut Vec<u8>,
	front_matter: &FrontMatter,
	parser: &mut Parser,
	input_file_path: &PathBuf,
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
			html::write_html(&mut output_buf, parser).unwrap_or_else(|e| {
				panic!(
					"Failed converting Markdown file \"{}\" to HTML: {}.",
					&input_file_path.display(),
					e
				)
			})
		}
		"date" => {
			write_to_output(&mut output_buf, front_matter.date.as_bytes())
		}
		"title" => {
			write_to_output(&mut output_buf, front_matter.title.as_bytes())
		}
		_ => {}
	}
	object.clear();
	field.clear();
}

fn handle_read(stream: &mut TcpStream) -> Option<ReadResult> {
	let mut buf = [0_u8; 4096];
	match stream.read(&mut buf) {
		Ok(size) => {
			if size == buf.len() {
				panic!("Request sizes as large as {} are not supported.", size)
			}

			let req_str = String::from_utf8_lossy(&buf);
			if req_str.len() == 0 {
				// Saw this occur once before adding the code path to avoid
				// panic! further down. Not sure about the cause of it.
				println!("WARNING: Invalid request? {:?}", &buf[0..=32]);
				return None;
			}

			println!("Request (size: {}):\n{}", size, req_str);
			let mut lines = req_str.lines();
			if let Some(first_line) = lines.next() {
				let mut components = first_line.split(' ');
				if let Some(method) = components.next() {
					if method == "GET" {
						if let Some(path) = components.next() {
							for line in lines {
								let mut components = line.split(' ');
								if let Some(component) = components.next() {
									if component == "Sec-WebSocket-Key:" {
										if let Some(websocket_key) =
											components.next()
										{
											return Some(
												ReadResult::WebSocket(
													String::from(websocket_key),
												),
											);
										}
									}
								}
							}

							Some(ReadResult::GetRequest(PathBuf::from(
								// Strip leading root slash.
								&path[1..],
							)))
						} else {
							panic!("Missing path in: {}", first_line)
						}
					} else {
						panic!(
							"Unsupported method \"{}\", line: {}",
							method, first_line
						)
					}
				} else {
					panic!(
						"Missing components in first HTTP request line: {}",
						first_line
					)
				}
			} else {
				panic!("Missing lines in HTTP request.")
			}
		}
		Err(e) => panic!("WARNING: Unable to read stream: {}", e),
	}
}

fn write(bytes: &[u8], stream: &mut TcpStream) {
	match stream.write_all(bytes) {
		Ok(()) => println!("Wrote {} bytes.", bytes.len()),
		Err(e) => println!("WARNING: Failed sending response: {}", e),
	}
}

fn handle_write(
	mut stream: TcpStream,
	path: &PathBuf,
	root_dir: &PathBuf,
	start_file: Option<PathBuf>,
) {
	let full_path = root_dir.join(&path);
	if full_path.is_file() {
		println!("Opening: {}", full_path.display());
		match fs::File::open(&full_path) {
			Ok(mut input_file) => {
				write(b"HTTP/1.1 200 OK\r\n", &mut stream);
				if let Some(extension) = path.extension() {
					let extension = extension.to_string_lossy();
					if extension == "html" {
						write(format!("Content-Type: text/{}; charset=UTF-8\r\n\r\n", extension).as_bytes(), &mut stream);
					}
				}
				let mut buf = [0_u8; 64 * 1024];
				loop {
					let size =
						input_file.read(&mut buf).unwrap_or_else(|e| {
							panic!(
								"Failed reading from {}: {}",
								full_path.display(),
								e
							);
						});
					if size < 1 {
						break;
					}

					write(&buf[0..=size], &mut stream);
				}
			}
			Err(e) => {
				match e.kind() {
					ErrorKind::NotFound => write(
						format!("HTTP/1.1 404 Not found\r\nContent-Type: text/html; charset=UTF-8\r\n\r\n<html><body>Couldn't find: {}</body></html>\r\n", full_path.display()).as_bytes(),
						&mut stream,
					),
					_ => write(
						format!("HTTP/1.1 500 Error\r\n{}", e)
							.as_bytes(),
						&mut stream,
					),
				}
			}
		}
	} else {
		println!("Requested path is not a file, returning index.");
		let iframe_src = if let Some(path) = start_file {
			let mut s = String::from(" src=\"");
			s.push_str(&path.to_string_lossy());
			s.push_str("\"");
			s
		} else {
			String::from("")
		};

		write(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=UTF-8\r\n\r\n<html>
<head><script>
// Tag on time in order to distinguish different sockets.
let socket = new WebSocket(\"ws://\" + window.location.hostname + \":\" + window.location.port + \"/chat?now=\" + Date.now())
socket.onopen = function(e) {{
//alert(\"[open] Connection established\")
}}
socket.onmessage = function(e) {{
e.data.text().then(text => window.frames['preview'].location.href = text)
}}
socket.onerror = function(e) {{
alert(`Socket error: ${{e}}`)
}}
window.addEventListener('beforeunload', (event) => {{
socket.close()
}});
</script>
<style type=\"text/css\">
BODY {{
font-family: \"Helvetica Neue\", Helvetica, Arial, sans-serif;
margin: 0;
}}
.banner {{
background: rgba(0, 0, 255, 0.2);
position: fixed;
}}
</style>
</head>
<body>
<div class=\"banner\">Preview, save Markdown file to disk for live reload:</div>
<iframe name=\"preview\"{} style=\"border:1px solid #eee; margin: 1px; width: 100%; height: 100%\"></iframe>
</body>
</html>\r\n", iframe_src).as_bytes(), &mut stream);
	}
}

fn handle_websocket(
	mut stream: TcpStream,
	key: &str,
	cond_pair: &Arc<(Mutex<Refresh>, Condvar)>,
) {
	// Based on WebSocket RFC - https://tools.ietf.org/html/rfc6455
	const FINAL_FRAME: u8 = 0b1000_0000;
	const BINARY_OPCODE: u8 = 0b0000_0010;
	const CLOSE_OPCODE: u8 = 0b0000_1000;
	const CLOSE_HEADER: u8 = FINAL_FRAME | CLOSE_OPCODE;
	const ZERO_LENGTH: u8 = 0;
	const CLOSE_FRAME: [u8; 2] = [CLOSE_HEADER, ZERO_LENGTH];

	let mut m = sha1::Sha1::new();
	m.update(key.as_bytes());
	m.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
	let accept_value = base64::encode(m.digest().bytes());

	write(format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\nSec-WebSocket-Protocol: chat\r\n\r\n", accept_value).as_bytes(), &mut stream);

	stream.set_nonblocking(true).expect(
		"Failed changing WebSocket TCP connection to nonblocking mode.",
	);

	let (mutex, cvar) = &**cond_pair;
	loop {
		let last_index = mutex
			.lock()
			.unwrap_or_else(|e| panic!("Failed locking mutex: {}", e))
			.index;
		let (guard, result) = cvar
			.wait_timeout_while(
				mutex
					.lock()
					.unwrap_or_else(|e| panic!("Failed locking mutex: {}", e)),
				Duration::from_millis(50),
				|pending| pending.index == last_index,
			)
			.unwrap_or_else(|e| panic!("Failed waiting: {}", e));

		if result.timed_out() {
			let mut buf = [0_u8; 16];
			let size =
				stream.read(&mut buf).unwrap_or_else(|e| match e.kind() {
					ErrorKind::WouldBlock => 0,
					_ => panic!("Failed reading: {}", e),
				});
			if size > 0 {
				// Is it a close frame?
				if buf[0] & 0b1000_1111 == CLOSE_HEADER {
					println!(
						"Received WebSocket connection close, responding in kind."
					);
					write(&CLOSE_FRAME, &mut stream);
					return;
				} else {
					println!(
						"WARNING: Received unhandled packet: {:?} ({} bytes)",
						&buf[0..size],
						size
					);
				}
			}
		} else {
			// Not a time-out? Then we got a proper file change notification!
			// Time to notify the browser.
			let message = if let Some(path) = &guard.file {
				String::from(path.to_string_lossy())
			} else {
				String::from("")
			};
			let length = message.len();
			if length > 125 {
				panic!("Don't support variable-length WebSocket frames yet.")
			}

			let frame = [FINAL_FRAME | BINARY_OPCODE, length.to_le_bytes()[0]];
			write(&frame, &mut stream);
			write(message.as_bytes(), &mut stream);
		}
	}
}

fn handle_client(
	mut stream: TcpStream,
	root_dir: &PathBuf,
	fs_cond: &Arc<(Mutex<Refresh>, Condvar)>,
	start_file: Option<PathBuf>,
) {
	if let Some(result) = handle_read(&mut stream) {
		match result {
			ReadResult::GetRequest(path) => {
				handle_write(stream, &path, root_dir, start_file)
			}
			ReadResult::WebSocket(key) => {
				handle_websocket(stream, &key, &fs_cond)
			}
		}
	}
}

fn parse_front_matter(
	input_file_path: &PathBuf,
	reader: &mut BufReader<fs::File>,
) -> FrontMatter {
	const MAX_FRONT_MATTER_LINES: u8 = 16;

	let mut result = FrontMatter {
		title: input_file_path
			.file_stem()
			.unwrap_or_else(|| panic!("Failed getting input file name."))
			.to_string_lossy()
			.to_string(),
		date: "1970-01-01T00:00:00Z".to_string(),
		published: true,
		edited: None,
		categories: Vec::new(),
		tags: Vec::new(),
		layout: None,
		custom_attributes: BTreeMap::new(),
	};

	let mut line = String::new();
	let first_line_len = reader.read_line(&mut line).unwrap_or_else(|e| {
		panic!(
			"Failed reading first line from \"{}\": {}.",
			input_file_path.display(),
			e
		)
	});

	// YAML Front matter present missing?
	if first_line_len != 4 || line != "---\n" {
		reader.seek(SeekFrom::Start(0)).unwrap_or_else(|e| {
			panic!(
				"Failed seeking in \"{}\": {}.",
				input_file_path.display(),
				e
			)
		});

		return result;
	}

	let mut front_matter_str = String::new();
	let mut line_count = 0;
	loop {
		line.clear();
		let _line_len = reader.read_line(&mut line).unwrap_or_else(|e| {
			panic!(
				"Failed reading line from \"{}\": {}.",
				input_file_path.display(),
				e
			)
		});
		if line == "---\n" {
			break;
		} else {
			line_count += 1;
			if line_count > MAX_FRONT_MATTER_LINES {
				panic!("Entered front matter parsing mode but failed to find end after {} lines while parsing {}.", MAX_FRONT_MATTER_LINES, input_file_path.display());
			}
			front_matter_str.push_str(&line);
		}
	}

	let yaml =
		YamlLoader::load_from_str(&front_matter_str).unwrap_or_else(|e| {
			panic!(
				"Failed loading YAML front matter from \"{}\": {}.",
				input_file_path.display(),
				e
			)
		});

	if yaml.len() != 1 {
		panic!("Expected only one YAML root element (Hash) in front matter of \"{}\" but got {}.", 
			input_file_path.display(), yaml.len());
	}

	if let yaml_rust::Yaml::Hash(hash) = &yaml[0] {
		for mapping in hash {
			if let yaml_rust::Yaml::String(s) = mapping.0 {
				parse_yaml_attribute(
					&mut result,
					&s,
					&mapping.1,
					input_file_path,
				)
			} else {
				panic!("Expected string keys in YAML element in front matter of \"{}\" but got {:?}.", 
						input_file_path.display(), &mapping.0)
			}
		}
	} else {
		panic!("Expected Hash as YAML root element in front matter of \"{}\" but got {:?}.", 
			input_file_path.display(), &yaml[0])
	}

	result
}

fn parse_yaml_attribute(
	front_matter: &mut FrontMatter,
	name: &str,
	value: &yaml_rust::Yaml,
	input_file_path: &PathBuf,
) {
	if name == "title" {
		if let yaml_rust::Yaml::String(value) = value {
			front_matter.title = value.clone();
		} else {
			panic!(
				"title of \"{}\" has unexpected type {:?}",
				input_file_path.display(),
				value
			)
		}
	} else if name == "date" {
		if let yaml_rust::Yaml::String(value) = value {
			front_matter.date = value.clone();
		} else {
			panic!(
				"date of \"{}\" has unexpected type {:?}",
				input_file_path.display(),
				value
			)
		}
	} else if name == "published" {
		if let yaml_rust::Yaml::Boolean(value) = value {
			front_matter.published = *value;
		} else {
			panic!(
				"published of \"{}\" has unexpected type {:?}",
				input_file_path.display(),
				value
			)
		}
	} else if name == "edited" {
		if let yaml_rust::Yaml::String(value) = value {
			front_matter.edited = Some(value.clone());
		} else {
			panic!(
				"edited of \"{}\" has unexpected type {:?}",
				input_file_path.display(),
				value
			)
		}
	} else if name == "categories" {
		if let yaml_rust::Yaml::Array(value) = value {
			for element in value {
				if let yaml_rust::Yaml::String(value) = element {
					front_matter.categories.push(value.clone())
				} else {
					panic!("Element of categories of \"{}\" has unexpected type {:?}",
						input_file_path.display(), element)
				}
			}
		} else {
			panic!(
				"categories of \"{}\" has unexpected type {:?}",
				input_file_path.display(),
				value
			)
		}
	} else if name == "tags" {
		if let yaml_rust::Yaml::Array(value) = value {
			for element in value {
				if let yaml_rust::Yaml::String(value) = element {
					front_matter.tags.push(value.clone())
				} else {
					panic!(
						"Element of tags of \"{}\" has unexpected type {:?}",
						input_file_path.display(),
						element
					)
				}
			}
		} else {
			panic!(
				"tags of \"{}\" has unexpected type {:?}",
				input_file_path.display(),
				value
			)
		}
	} else if name == "layout" {
		if let yaml_rust::Yaml::String(value) = value {
			front_matter.layout = Some(value.clone());
		} else {
			panic!(
				"layout of \"{}\" has unexpected type {:?}",
				input_file_path.display(),
				value
			)
		}
	} else if let yaml_rust::Yaml::String(value) = value {
		front_matter
			.custom_attributes
			.insert(name.to_string(), value.clone());
	} else {
		panic!(
			"custom attribute \"{}\" of \"{}\" has unexpected type {:?}",
			name,
			input_file_path.display(),
			value
		)
	}
}
