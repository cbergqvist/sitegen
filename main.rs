use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::{BufRead, ErrorKind, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::option::Option;
use std::path::PathBuf;
use std::string::String;
use std::sync::mpsc::channel;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use std::{env, fmt, fs, io};

use pulldown_cmark::{html, Parser};

use notify::{watcher, RecursiveMode, Watcher};

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

struct StringArg {
	name: &'static str,
	help: &'static str,
	value: String,
}

impl fmt::Display for BoolArg {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "-{} {}", self.name, self.help)
	}
}

impl fmt::Display for StringArg {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "-{} {}", self.name, self.help)
	}
}

fn main() -> io::Result<()> {
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
	};
	let mut input_arg = StringArg {
		name: "input",
		help: "Set input directory to process.",
		value: String::from("./input"),
	};
	let mut output_arg = StringArg {
		name: "output",
		help: "Set output directory to write to.",
		value: String::from("./output"),
	};
	let mut watch_arg = BoolArg {
		name: "watch",
		help: "Run indefinitely, watching input directory for changes.",
		value: false,
	};

	let mut first_arg = true;
	let mut previous_arg = None;
	for mut arg in env::args() {
		// Skip executable arg itself.
		if first_arg {
			first_arg = false;
			continue;
		}

		if let Some(prev) = previous_arg {
			if prev == host_arg.name {
				host_arg.value = arg;
			} else if prev == input_arg.name {
				input_arg.value = arg;
			} else if prev == output_arg.name {
				output_arg.value = arg;
			}
			previous_arg = None;
			continue;
		}

		if arg.len() < 3
			|| arg.as_bytes()[0] != b'-'
			|| arg.as_bytes()[1] != b'-'
		{
			panic!("Unexpected argument: {}", arg)
		}

		arg = arg.split_off(2);

		if arg == help_arg.name {
			help_arg.value = true;
		} else if arg == host_arg.name
			|| arg == input_arg.name
			|| arg == output_arg.name
		{
			previous_arg = Some(arg);
		} else if arg == watch_arg.name {
			watch_arg.value = true;
		} else {
			panic!("Unsupported argument: {}", arg)
		}
	}

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
		println!("{}", watch_arg);

		return Ok(());
	}

	let markdown_extension = OsStr::new("md");

	let markdown_files =
		get_markdown_files(&input_arg.value, markdown_extension);

	let mut output_files = Vec::new();

	if markdown_files.is_empty() {
		println!("Found no valid file entries under \"{}\".", input_arg.value);
	} else {
		fs::create_dir(&output_arg.value).unwrap_or_else(|e| {
			panic!("Failed creating \"{}\": {}.", output_arg.value, e)
		});

		for file_name in &markdown_files {
			output_files.push(process_markdown_file(
				&file_name,
				&input_arg.value,
				&output_arg.value,
			))
		}
	}

	if !watch_arg.value {
		return Ok(());
	}

	const PORT: i16 = 8090;
	let listener = TcpListener::bind(format!("{}:{}", host_arg.value, PORT))
		.unwrap_or_else(|e| panic!("Failed to bind TCP listening port: {}", e));
	println!("Listening for connections on {}:{}", host_arg.value, PORT);

	struct Refresh {
		index: u32,
		file: Option<PathBuf>,
	}

	let cond_pair = Arc::new((
		Mutex::new(Refresh {
			index: 0,
			file: None,
		}),
		Condvar::new(),
	));

	enum ReadResult {
		GetRequest(PathBuf),
		WebSocket(String),
	}

	fn handle_read(stream: &mut TcpStream) -> ReadResult {
		let mut buf = [0u8; 4096];
		match stream.read(&mut buf) {
			Ok(size) => {
				if size == buf.len() {
					panic!(
						"Request sizes as large as {} are not supported.",
						size
					)
				}

				let req_str = String::from_utf8_lossy(&buf);
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
												return ReadResult::WebSocket(
													String::from(websocket_key),
												);
											}
										}
									}
								}

								return ReadResult::GetRequest(PathBuf::from(
									// Strip leading root slash.
									&path[1..],
								));
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
		path: PathBuf,
		root_dir: &PathBuf,
		start_file: Option<PathBuf>,
		host: &str,
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
					let mut buf = [0u8; 64 * 1024];
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

						write(&buf[0..size + 1], &mut stream);
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
let socket = new WebSocket(\"ws://{}:{}/chat?now=\" + Date.now())
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
</script></head>
<body style=\"margin: 1px;\">
<div style=\"display: block; position: fixed; background: rgba(0, 0, 255, 0.2); width: 100%\">Preview, save Markdown file to disk for live reload:</div>
<iframe name=\"preview\"{} style=\"border:1px solid #eee; margin: 1px; width: 100%; height: 100%\"></iframe>
</body>
</html>\r\n", host, PORT, iframe_src).as_bytes(), &mut stream);
		}
	}

	fn handle_websocket(
		mut stream: TcpStream,
		key: String,
		cond_pair: Arc<(Mutex<Refresh>, Condvar)>,
	) {
		let mut m = sha1::Sha1::new();
		m.update(key.as_bytes());
		m.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
		let accept_value = base64::encode(m.digest().bytes());

		write(format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\nSec-WebSocket-Protocol: chat\r\n\r\n", accept_value).as_bytes(), &mut stream);

		stream.set_nonblocking(true).expect(
			"Failed changing WebSocket TCP connection to nonblocking mode.",
		);

		let (mutex, cvar) = &*cond_pair;
		loop {
			let last_index = mutex
				.lock()
				.unwrap_or_else(|e| panic!("Failed locking mutex: {}", e))
				.index;
			let (guard, result) = cvar
				.wait_timeout_while(
					mutex.lock().unwrap_or_else(|e| {
						panic!("Failed locking mutex: {}", e)
					}),
					Duration::from_millis(50),
					|pending| pending.index == last_index,
				)
				.unwrap_or_else(|e| panic!("Failed waiting: {}", e));

			// Based on WebSocket RFC - https://tools.ietf.org/html/rfc6455
			const FINAL_FRAME: u8 = 0b1000_0000;
			const BINARY_OPCODE: u8 = 0b0000_0010;
			const CLOSE_OPCODE: u8 = 0b0000_1000;
			if result.timed_out() {
				let mut buf = [0u8; 16];
				let size =
					stream.read(&mut buf).unwrap_or_else(|e| match e.kind() {
						ErrorKind::WouldBlock => 0,
						_ => panic!("Failed reading: {}", e),
					});
				if size > 0 {
					// Is it a close frame?
					const CLOSE_FRAME: u8 = FINAL_FRAME | CLOSE_OPCODE;
					if buf[0] & 0b1000_1111 == CLOSE_FRAME {
						println!(
							"Received WebSocket connection close, responding in kind."
						);
						const LENGTH: u8 = 0;
						let frame = [CLOSE_FRAME, LENGTH];
						write(&frame, &mut stream);
						return;
					} else {
						println!("WARNING: Received unhandled packet: {:?} ({} bytes)", &buf[0..size], size);
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
				if message.len() > 125 {
					panic!(
						"Don't support variable-length WebSocket frames yet."
					)
				}
				let frame = [FINAL_FRAME | BINARY_OPCODE, message.len() as u8];
				write(&frame, &mut stream);
				write(message.as_bytes(), &mut stream);
			}
		}
	}

	fn handle_client(
		mut stream: TcpStream,
		root_dir: &PathBuf,
		cond_pair: Arc<(Mutex<Refresh>, Condvar)>,
		start_file: Option<PathBuf>,
		host: &str,
	) {
		match handle_read(&mut stream) {
			ReadResult::GetRequest(path) => {
				handle_write(stream, path, root_dir, start_file, host)
			}
			ReadResult::WebSocket(key) => {
				handle_websocket(stream, key, cond_pair)
			}
		}
	}

	let root_dir = PathBuf::from(&output_arg.value);
	let fs_cond_pair_clone = cond_pair.clone();
	let start_file = output_files.first().cloned();

	let listening_thread = thread::spawn(move || {
		for stream in listener.incoming() {
			match stream {
				Ok(stream) => {
					let root_dir_clone = root_dir.clone();
					let cond_pair_clone = cond_pair.clone();
					let start_file_clone = start_file.clone();
					let host = host_arg.value.clone();
					thread::spawn(move || {
						handle_client(
							stream,
							&root_dir_clone,
							cond_pair_clone,
							start_file_clone,
							&host,
						)
					});
				}
				Err(e) => println!("WARNING: Unable to connect: {}", e),
			}
		}
		println!("{:?}", start_file);
	});

	let fs_thread = thread::spawn(move || {
		let (tx, rx) = channel();
		let mut watcher = watcher(tx, Duration::from_millis(200))
			.unwrap_or_else(|e| {
				panic!("Unable to create watcher: {}", e);
			});

		watcher
			.watch(&input_arg.value, RecursiveMode::Recursive)
			.unwrap_or_else(|e| {
				panic!("Unable to watch {}: {}", &input_arg.value, e);
			});

		loop {
			match rx.recv() {
				Ok(event) => match event {
					notify::DebouncedEvent::Write(mut path)
					| notify::DebouncedEvent::Create(mut path) => {
						path = path.canonicalize().unwrap_or_else(|e| {
							panic!(
								"Canonicalization of {} failed: {}",
								path.display(),
								e
							)
						});
						let path_to_communicate = if is_file_with_extension(
							&path,
							&markdown_extension,
						) {
							match fs::create_dir(&output_arg.value) {
								Ok(_) => {}
								Err(e) => {
									if e.kind() != ErrorKind::AlreadyExists {
										panic!(
											"Failed creating \"{}\": {}.",
											output_arg.value, e
										)
									}
								}
							}

							Some(process_markdown_file(
								&path,
								&input_arg.value,
								&output_arg.value,
							))
						} else {
							None
						};

						let (mutex, cvar) = &*fs_cond_pair_clone;

						let mut refresh = mutex.lock().unwrap_or_else(|e| {
							panic!("Failed locking mutex: {}", e)
						});
						if path_to_communicate.is_some() {
							refresh.file = path_to_communicate;
						}
						refresh.index += 1;
						cvar.notify_all();
					}
					_ => {
						println!("Skipping {:?}", event);
					}
				},
				Err(e) => panic!("Watch error: {}", e),
			}
		}
	});

	listening_thread
		.join()
		.expect("Failed joining listening thread.");

	fs_thread.join().expect("Failed joining FS thread.");

	Ok(())
}

fn is_file_with_extension(path: &PathBuf, extension: &OsStr) -> bool {
	path.extension() == Some(extension)
}

fn get_markdown_files(
	input_path: &str,
	markdown_extension: &OsStr,
) -> Vec<PathBuf> {
	let entries = fs::read_dir(input_path).unwrap_or_else(|e| {
		panic!("Failed reading paths from \"{}\": {}.", input_path, e)
	});
	let mut files = Vec::new();
	for entry in entries {
		match entry {
			Ok(entry) => {
				let path = entry.path();
				if is_file_with_extension(&path, markdown_extension) {
					if let Ok(ft) = entry.file_type() {
						if ft.is_file() {
							files.push(path);
							println!(
								"Markdown!: \"{}\"",
								entry.path().display()
							);
						} else {
							println!("WARNING: Non-file named .md?");
						}
					} else {
						println!(
							"WARNING: Failed getting file type of {}.",
							entry.path().display()
						);
					}
				} else {
					println!(
						"Skipping non-.md file: \"{}\"",
						entry.path().display()
					);
				}
			}
			Err(e) => {
				println!("WARNING: Invalid entry in \"{}\": {}", input_path, e)
			}
		}
	}

	files
}

fn process_markdown_file(
	input_file_name: &PathBuf,
	input_path: &str,
	output_path: &str,
) -> PathBuf {
	fn write_to_output(
		output_buf: &mut io::BufWriter<&mut Vec<u8>>,
		data: &[u8],
	) {
		output_buf.write_all(data).unwrap_or_else(|e| {
			panic!("Failed writing \"{:?}\" to to buffer: {}.", data, e)
		});
	}

	let timer = Instant::now();
	let input_file = fs::File::open(&input_file_name).unwrap_or_else(|e| {
		panic!("Failed opening \"{}\": {}.", &input_file_name.display(), e)
	});

	let input_file_name_str = input_file_name.to_str().unwrap_or_else(|| {
		panic!(
			"Failed converting \"{}\" to str.",
			&input_file_name.display()
		)
	});

	let mut reader = io::BufReader::new(input_file);

	let front_matter =
		parse_front_matter(&input_file_name_str, &mut reader, input_path);
	let mut input_file_str = String::new();
	let _size =
		reader
			.read_to_string(&mut input_file_str)
			.unwrap_or_else(|e| {
				panic!(
					"Failed reading first line from \"{}\": {}.",
					&input_file_name.display(),
					e
				)
			});
	let parser = Parser::new(&input_file_str);
	let mut output = Vec::new();
	let mut output_buf = io::BufWriter::new(&mut output);
	write_to_output(
		&mut output_buf,
		b"<html>
<head>
<title>",
	);
	write_to_output(&mut output_buf, front_matter.title.as_bytes());
	write_to_output(
		&mut output_buf,
		b"</title>
<style type=\"text/css\">
.container {
	max-width: 38rem;
	margin-left: auto;
	margin-right: auto;
	font-family: \"Helvetica Neue\", Helvetica, Arial, sans-serif;
}
TIME {
	color: rgb(154, 154, 154);
}
HR {
	border: 0;
	border-top: 1px solid #eee;
}
</style>
</head>
<body>
<div class=\"container\">
<time datetime=\"",
	);
	write_to_output(&mut output_buf, front_matter.date.as_bytes());
	write_to_output(&mut output_buf, b"\">");
	write_to_output(&mut output_buf, front_matter.date.as_bytes());
	write_to_output(&mut output_buf, b"</time>");
	html::write_html(&mut output_buf, parser).unwrap_or_else(|e| {
		panic!(
			"Failed converting Markdown file \"{}\" to HTML: {}.",
			&input_file_name.display(),
			e
		)
	});
	write_to_output(
		&mut output_buf,
		b"</div>
</body>
</html>",
	);

	let mut output_file_name = String::from(output_path);
	if input_file_name.starts_with(input_path) {
		output_file_name.push_str(
			&input_file_name_str
				[input_path.len()..(input_file_name_str.len() - "md".len())],
		);
	} else {
		let full_input_path =
			fs::canonicalize(input_path).unwrap_or_else(|e| {
				panic!("Failed to canonicalize {}: {}", input_path, e)
			});
		if input_file_name.starts_with(&full_input_path) {
			let full_input_path_str =
				full_input_path.to_str().unwrap_or_else(|| {
					panic!(
						"Failed to convert {} into string.",
						full_input_path.display()
					);
				});
			output_file_name.push_str(
				&input_file_name_str[full_input_path_str.len()
					..(input_file_name_str.len() - "md".len())],
			);
		} else {
			panic!(
				"Unable to handle input file name: {}",
				input_file_name.display()
			)
		}
	}
	output_file_name.push_str("html");

	let mut output_file =
		fs::File::create(&output_file_name).unwrap_or_else(|e| {
			panic!("Failed creating \"{}\": {}.", &output_file_name, e)
		});
	output_file
		.write_all(&output_buf.buffer())
		.unwrap_or_else(|e| {
			panic!("Failed writing to \"{}\": {}.", &output_file_name, e)
		});

	// Avoiding sync_all() for now to be friendlier to disks.
	output_file.sync_data().unwrap_or_else(|e| {
		panic!("Failed sync_data() for \"{}\": {}.", &output_file_name, e)
	});

	println!(
		"Done with {} after {} ms.",
		input_file_name_str,
		timer.elapsed().as_millis()
	);

	PathBuf::from(&output_file_name[output_path.len()..])
}

fn parse_front_matter(
	input_file_name: &str,
	reader: &mut io::BufReader<fs::File>,
	input_path: &str,
) -> FrontMatter {
	let mut result = FrontMatter {
		title: input_file_name[input_path.len() + 1..input_file_name.len() - 3]
			.to_owned(),
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
			&input_file_name, e
		)
	});

	// YAML Front matter present missing?
	if first_line_len != 4 || line != "---\n" {
		reader.seek(SeekFrom::Start(0)).unwrap_or_else(|e| {
			panic!("Failed seeking in \"{}\": {}.", &input_file_name, e)
		});

		return result;
	}

	use yaml_rust::YamlLoader;

	let mut front_matter_str = String::new();
	const MAX_FRONT_MATTER_LINES: u8 = 16;
	let mut line_count = 0;
	loop {
		line.clear();
		let _line_len = reader.read_line(&mut line).unwrap_or_else(|e| {
			panic!("Failed reading line from \"{}\": {}.", &input_file_name, e)
		});
		if line == "---\n" {
			break;
		} else {
			line_count += 1;
			if line_count > MAX_FRONT_MATTER_LINES {
				panic!("Entered front matter parsing mode but failed to find end after {} lines while parsing {}.", MAX_FRONT_MATTER_LINES, &input_file_name);
			}
			front_matter_str.push_str(&line);
		}
	}

	let yaml =
		YamlLoader::load_from_str(&front_matter_str).unwrap_or_else(|e| {
			panic!(
				"Failed loading YAML front matter from \"{}\": {}.",
				&input_file_name, e
			)
		});

	if yaml.len() != 1 {
		panic!("Expected only one YAML root element (Hash) in front matter of \"{}\" but got {}.", 
			&input_file_name, yaml.len());
	}

	if let yaml_rust::Yaml::Hash(hash) = &yaml[0] {
		for mapping in hash {
			if let yaml_rust::Yaml::String(s) = mapping.0 {
				parse_yaml_attribute(
					&mut result,
					&s,
					&mapping.1,
					&input_file_name,
				)
			} else {
				panic!("Expected string keys in YAML element in front matter of \"{}\" but got {:?}.", 
						&input_file_name, &mapping.0)
			}
		}
	} else {
		panic!("Expected Hash as YAML root element in front matter of \"{}\" but got {:?}.", 
			&input_file_name, &yaml[0])
	}

	result
}

fn parse_yaml_attribute(
	front_matter: &mut FrontMatter,
	name: &str,
	value: &yaml_rust::Yaml,
	input_file_name: &str,
) {
	if name == "title" {
		if let yaml_rust::Yaml::String(value) = value {
			front_matter.title = value.clone();
		} else {
			panic!(
				"title of \"{}\" has unexpected type {:?}",
				&input_file_name, value
			)
		}
	} else if name == "date" {
		if let yaml_rust::Yaml::String(value) = value {
			front_matter.date = value.clone();
		} else {
			panic!(
				"date of \"{}\" has unexpected type {:?}",
				&input_file_name, value
			)
		}
	} else if name == "published" {
		if let yaml_rust::Yaml::Boolean(value) = value {
			front_matter.published = *value;
		} else {
			panic!(
				"published of \"{}\" has unexpected type {:?}",
				&input_file_name, value
			)
		}
	} else if name == "edited" {
		if let yaml_rust::Yaml::String(value) = value {
			front_matter.edited = Some(value.clone());
		} else {
			panic!(
				"edited of \"{}\" has unexpected type {:?}",
				&input_file_name, value
			)
		}
	} else if name == "categories" {
		if let yaml_rust::Yaml::Array(value) = value {
			for element in value {
				if let yaml_rust::Yaml::String(value) = element {
					front_matter.categories.push(value.clone())
				} else {
					panic!("Element of categories of \"{}\" has unexpected type {:?}",
						&input_file_name, element)
				}
			}
		} else {
			panic!(
				"categories of \"{}\" has unexpected type {:?}",
				&input_file_name, value
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
						&input_file_name, element
					)
				}
			}
		} else {
			panic!(
				"tags of \"{}\" has unexpected type {:?}",
				&input_file_name, value
			)
		}
	} else if name == "layout" {
		if let yaml_rust::Yaml::String(value) = value {
			front_matter.layout = Some(value.clone());
		} else {
			panic!(
				"layout of \"{}\" has unexpected type {:?}",
				&input_file_name, value
			)
		}
	} else if let yaml_rust::Yaml::String(value) = value {
		front_matter
			.custom_attributes
			.insert(name.to_string(), value.clone());
	} else {
		panic!(
			"custom attribute \"{}\" of \"{}\" has unexpected type {:?}",
			name, &input_file_name, value
		)
	}
}
