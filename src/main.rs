use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::{ErrorKind, Read};
use std::net::{TcpListener, TcpStream};
use std::option::Option;
use std::path::PathBuf;
use std::string::String;
use std::sync::mpsc::channel;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;
use std::{env, fmt, fs};

use notify::{watcher, RecursiveMode, Watcher};

mod atom;
mod front_matter;
mod markdown;
mod util;
mod websocket;

use util::{write, Refresh};

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

enum ReadResult {
	GetRequest(PathBuf),
	WebSocket(String),
}

fn main() {
	// Not using the otherwise brilliant CLAP crate since I detest string
	// matching args to get their values.
	let mut author_arg = StringArg {
		name: "author",
		help: "Set the name of the author.",
		value: String::from("John Doe"),
		set: false,
	};
	let mut base_url_arg = StringArg {
		name: "base_url",
		help: "Set base URL to be used in output files, default is \"http://test.com/\".",
		value: String::from("http://test.com/"),
		set: false,
	};
	let mut email_arg = StringArg {
		name: "email",
		help: "Set email of the author.",
		value: String::from("john.doe@test.com"),
		set: false,
	};
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
		&mut vec![
			&mut author_arg,
			&mut base_url_arg,
			&mut email_arg,
			&mut host_arg,
			&mut input_arg,
			&mut output_arg,
		],
	);

	if help_arg.value {
		println!(
			"SiteGen version 0.1
Christopher Bergqvist <chris@digitalpoetry.se>

Basic static site generator.

Arguments:"
		);
		println!("{}", author_arg);
		println!("{}", base_url_arg);
		println!("{}", email_arg);
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
		&author_arg.value,
		&email_arg.value,
		&base_url_arg.value,
	)
}

fn inner_main(
	input_dir: &PathBuf,
	output_dir: &PathBuf,
	host: &str,
	port: i16,
	watch: bool,
	author: &str,
	email: &str,
	base_url: &str,
) {
	let markdown_extension = OsStr::new("md");

	let mut output_files = Vec::new();

	let markdown_files = markdown::get_files(input_dir, markdown_extension);

	if markdown_files.is_empty() {
		println!(
			"Found no valid file entries under \"{}\".",
			input_dir.display()
		);
	} else {
		fs::create_dir(&output_dir).unwrap_or_else(|e| {
			panic!("Failed creating \"{}\": {}.", output_dir.display(), e)
		});

		let mut groups = HashMap::new();
		for file_name in &markdown_files {
			let generated =
				markdown::process_file(file_name, input_dir, output_dir);
			if let Some(group) = generated.group {
				let entries = groups.entry(group).or_insert_with(Vec::new);
				entries.push(atom::FeedEntry {
					front_matter: generated.front_matter,
					html_content: generated.html_content,
					permalink: generated.path.clone(),
				});
			}
			output_files.push(generated.path)
		}

		for (group, entries) in groups {
			let mut feed_name = output_dir.join(PathBuf::from(&group));
			feed_name.set_extension("xml");
			let header = atom::FeedHeader {
				title: group,
				base_url: base_url.to_string(),
				latest_update: "2001-01-19T20:10:00Z".to_string(),
				author_name: author.to_string(),
				author_email: email.to_string(),
			};
			atom::generate(&feed_name, &header, entries, output_dir);
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
	watch_fs(input_dir, output_dir, markdown_extension, &fs_cond_clone);

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

	let listener_builder =
		thread::Builder::new().name("TCP_listener".to_string());
	listener_builder
		.spawn(move || {
			for stream in listener.incoming() {
				match stream {
					Ok(stream) => {
						let root_dir_clone = root_dir.clone();
						let fs_cond_pair_clone = fs_cond.clone();
						let start_file_clone = start_file.clone();
						let stream_builder = thread::Builder::new()
							.name("TCP_stream".to_string());
						stream_builder
							.spawn(move || {
								handle_client(
									stream,
									&root_dir_clone,
									&fs_cond_pair_clone,
									start_file_clone,
								)
							})
							.unwrap_or_else(|e| {
								panic!(
									"Failed spawning TCP stream thread: {}",
									e
								)
							});
					}
					Err(e) => println!("WARNING: Unable to connect: {}", e),
				}
			}
		})
		.unwrap_or_else(|e| {
			panic!("Failed spawning TCP listening thread: {}", e)
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
						println!(
							"Path to communicate: {:?}",
							path_to_communicate
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

		return Some(markdown::process_file(&path, input_dir, output_dir).path);
	} else if path.extension() == Some(html_extension) {
		let parent_path = path.parent().unwrap_or_else(|| {
			panic!("Path without a parent directory?: {}", path.display())
		});
		let parent_path_file_name =
			parent_path.file_name().unwrap_or_else(|| {
				panic!("Missing file name in path: {}", parent_path.display())
			});
		if parent_path_file_name == "_layouts" {
			let file_stem = path.file_stem().unwrap_or_else(|| {
				panic!("Missing file stem in path: {}", path.display())
			});
			let mut dir_name = OsString::from(file_stem);
			dir_name.push("s");
			let markdown_dir = input_dir.join(dir_name);
			let markdown_files = if markdown_dir.exists() {
				markdown::get_files(&markdown_dir, markdown_extension)
			} else {
				Vec::new()
			};

			if markdown_files.is_empty() {
				let templated_file = input_dir
					.join(file_stem)
					.with_extension(markdown_extension);
				if templated_file.exists() {
					return Some(
						markdown::process_file(
							&templated_file,
							input_dir,
							output_dir,
						)
						.path,
					);
				}
			} else {
				let mut output_files = Vec::new();
				for file_name in &markdown_files {
					output_files.push(markdown::process_file(
						file_name, input_dir, output_dir,
					))
				}
				return output_files.first().map(|g| g.path.clone());
			}
		} else if parent_path_file_name == "_includes" {
			// Since we don't track what includes what, just do a full refresh.
			let markdown_files =
				markdown::get_files(input_dir, markdown_extension);
			for file_name in &markdown_files {
				markdown::process_file(file_name, input_dir, output_dir);
			}
			// Special identifier making JavaScript reload the current page.
			return Some(PathBuf::from("*"));
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

fn handle_read(stream: &mut TcpStream) -> Option<ReadResult> {
	let mut buf = [0_u8; 4096];
	match stream.read(&mut buf) {
		Ok(size) => {
			if size == buf.len() {
				panic!("Request sizes as large as {} are not supported.", size)
			} else if size == 0 {
				// Seen this occur a few times with zero-filled buf.
				// Not sure about the cause of it.
				println!("Zero-size TCP stream read()-result. Ignoring.");
				return None;
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
e.data.text().then(text => {{ if (text == \"*\") {{ window.frames['preview'].location.reload() }} else {{ window.frames['preview'].location.href = text }} }})
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
<iframe name=\"preview\"{} style=\"border: 0; margin: 0; width: 100%; height: 100%\"></iframe>
</body>
</html>\r\n", iframe_src).as_bytes(), &mut stream);
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
				websocket::handle_stream(stream, &key, fs_cond)
			}
		}
	}
}
