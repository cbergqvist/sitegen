use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::Write;
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

use markdown::ComputedFilePath;
use util::{write_to_stream_log_count, Refresh};

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

// Not using the otherwise brilliant CLAP crate since I detest string matching
// args to get their values.
struct ConfigArgs {
	author: StringArg,
	base_url: StringArg,
	email: StringArg,
	help: BoolArg, // Command line-only, doesn't transfer into Config.
	host: StringArg,
	input: StringArg,
	output: StringArg,
	port: I16Arg,
	watch: BoolArg,
}

struct Config {
	author: String,
	base_url: String,
	email: String,
	host: String,
	input_dir: PathBuf,
	output_dir: PathBuf,
	port: i16,
	watch: bool,
}

impl ConfigArgs {
	fn new() -> Self {
		Self {
			author: StringArg {
				name: "author",
				help: "Set the name of the author.",
				value: String::from("John Doe"),
				set: false,
			},
			base_url: StringArg {
				name: "base_url",
				help: "Set base URL to be used in output files, default is \"http://test.com/\".",
				value: String::from("http://test.com/"),
				set: false,
			},
			email: StringArg {
				name: "email",
				help: "Set email of the author.",
				value: String::from("john.doe@test.com"),
				set: false,
			},
			help: BoolArg {
				name: "help",
				help: "Print this text.",
				value: false,
			},
			host: StringArg {
				name: "host",
				help: "Set address to bind to. The default 127.0.0.1 can be used for privacy and 0.0.0.0 to give access to other machines.",
				value: String::from("127.0.0.1"),
				set: false,
			},
			input: StringArg {
				name: "input",
				help: "Set input directory to process.",
				value: String::from("./input"),
				set: false,
			},
			output: StringArg {
				name: "output",
				help: "Set output directory to write to.",
				value: String::from("./output"),
				set: false,
			},
			port: I16Arg {
				name: "port",
				help: "Set port to bind to.",
				value: 8090,
				set: false,
			},
			watch: BoolArg {
				name: "watch",
				help: "Run indefinitely, watching input directory for changes.",
				value: false,
			},
		}
	}

	fn parse(&mut self, args: std::env::Args) {
		let mut bool_args = vec![&mut self.help, &mut self.watch];
		let mut i16_args = vec![&mut self.port];
		let mut string_args = vec![
			&mut self.author,
			&mut self.base_url,
			&mut self.email,
			&mut self.host,
			&mut self.input,
			&mut self.output,
		];

		let mut first_arg = true;
		let mut previous_arg = None;
		'arg_loop: for mut arg in args {
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
						i16_arg.value =
							arg.parse::<i16>().unwrap_or_else(|e| {
								panic!(
									"Invalid value for {}: {}",
									i16_arg.name, e
								);
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

	fn values(self) -> Config {
		Config {
			author: self.author.value,
			base_url: self.base_url.value,
			email: self.email.value,
			host: self.host.value,
			input_dir: PathBuf::from(self.input.value),
			output_dir: PathBuf::from(self.output.value),
			port: self.port.value,
			watch: self.watch.value,
		}
	}
}

fn main() {
	let mut args = ConfigArgs::new();
	args.parse(env::args());

	if args.help.value {
		println!(
			"SiteGen version 0.1
Christopher Bergqvist <chris@digitalpoetry.se>

Basic static site generator.

Arguments:"
		);
		println!("{}", args.author);
		println!("{}", args.base_url);
		println!("{}", args.email);
		println!("{}", args.help);
		println!("{}", args.host);
		println!("{}", args.input);
		println!("{}", args.output);
		println!("{}", args.port);
		println!("{}", args.watch);

		return;
	}

	if !args.watch.value && (args.host.set || args.port.set) {
		println!(
			"WARNING: {} or {} arg set without {} arg, so they have no use.",
			args.host.name, args.port.name, args.watch.name
		)
	}

	inner_main(&args.values())
}

fn inner_main(config: &Config) {
	let input_files = markdown::get_files(&config.input_dir);
	let mut input_output_map = HashMap::new();

	if input_files.is_empty() {
		println!(
			"Found no valid file entries under \"{}\".",
			config.input_dir.display()
		);
	} else {
		fs::create_dir(&config.output_dir).unwrap_or_else(|e| {
			panic!(
				"Failed creating \"{}\": {}.",
				config.output_dir.display(),
				e
			)
		});

		// First, build up the input -> output map so that later when we do
		// actual processing of files, we have records of all other files.
		// This allows us to properly detect broken links.
		for file_name in &input_files.html {
			let output_file = markdown::compute_output_path(
				file_name,
				&config.input_dir,
				&config.output_dir,
			);
			checked_insert(
				file_name.clone(),
				output_file,
				&mut input_output_map,
			)
		}
		for file_name in &input_files.markdown {
			let output_file = markdown::compute_output_path(
				file_name,
				&config.input_dir,
				&config.output_dir,
			);
			checked_insert(
				file_name.clone(),
				output_file,
				&mut input_output_map,
			)
		}
		for file_name in &input_files.raw {
			let output_file_path = config
				.output_dir
				.join(file_name.strip_prefix(&config.input_dir).unwrap());
			checked_insert(
				file_name.clone(),
				ComputedFilePath {
					path: output_file_path,
					group: None,
				},
				&mut input_output_map,
			)
		}

		let mut groups = HashMap::new();
		for (_, output_file) in &input_output_map {
			if let Some(group) = &output_file.group {
				match groups.entry(group.to_string()) {
					Entry::Occupied(..) => {}
					Entry::Vacant(ve) => {
						ve.insert(Vec::new());
					}
				}
			}
		}

		for (group, _) in &groups {
			let xml_file = PathBuf::from("feeds")
				.join(PathBuf::from(group).with_extension("xml"));
			checked_insert(
				config.input_dir.join(&xml_file), // virtual input
				ComputedFilePath {
					path: config.output_dir.join(xml_file),
					group: None,
				},
				&mut input_output_map,
			)
		}

		for file_name in &input_files.html {
			let _path = markdown::process_template_file_without_markdown(
				file_name,
				&config.input_dir,
				&config.output_dir,
				&mut input_output_map,
			);
		}

		for file_name in &input_files.markdown {
			let generated = markdown::process_file(
				file_name,
				&config.input_dir,
				&config.output_dir,
				&mut input_output_map,
			);
			if let Some(group) = generated.group {
				match groups.entry(group.clone()) {
					Entry::Vacant(..) => panic!(
						"Group {} should have already been added above.",
						group
					),
					Entry::Occupied(oe) => {
						let entries = oe.into_mut();
						entries.push(atom::FeedEntry {
							front_matter: generated.front_matter,
							html_content: generated.html_content,
							permalink: generated.path.clone(),
						});
					}
				}
			}
		}

		for (group, entries) in groups {
			let feed_name = config
				.output_dir
				.join(PathBuf::from("feeds").join(PathBuf::from(&group)))
				.with_extension("xml");
			let header = atom::FeedHeader {
				title: group.to_string(),
				base_url: config.base_url.to_string(),
				latest_update: "2001-01-19T20:10:00Z".to_string(),
				author_name: config.author.to_string(),
				author_email: config.email.to_string(),
			};
			atom::generate(&feed_name, &header, entries, &config.output_dir);
		}

		util::copy_files_with_prefix(
			&input_files.raw,
			&config.input_dir,
			&config.output_dir,
		)
	}

	if !config.watch {
		return;
	}

	let fs_cond = Arc::new((
		Mutex::new(Refresh {
			index: 0,
			file: None,
		}),
		Condvar::new(),
	));

	let root_dir = PathBuf::from(&config.output_dir);
	let fs_cond_clone = fs_cond.clone();
	let start_file = find_newest_file(&input_output_map, &config.input_dir);

	spawn_listening_thread(
		&config.host,
		config.port,
		root_dir,
		fs_cond,
		start_file,
	);

	// As we start watching some time after we've done initial processing, it is
	// possible that files get modified in between and changes get lost.
	watch_fs(
		&config.input_dir,
		&config.output_dir,
		&fs_cond_clone,
		&mut input_output_map,
	);
}

fn checked_insert(
	key: PathBuf,
	value: ComputedFilePath,
	map: &mut HashMap<PathBuf, ComputedFilePath>,
) {
	match map.entry(key) {
		Entry::Occupied(oe) => {
			panic!(
				"Key {} already had value: {}, when trying to insert: {}",
				oe.key().display(),
				oe.get().path.display(),
				value.path.display()
			);
		}
		Entry::Vacant(ve) => ve.insert(value),
	};
}

fn find_newest_file(
	input_output_map: &HashMap<PathBuf, ComputedFilePath>,
	input_dir: &PathBuf,
) -> Option<PathBuf> {
	let mut newest_file = None;
	let mut newest_time = std::time::UNIX_EPOCH;
	let virtual_dir = input_dir.join(PathBuf::from("feeds"));
	for (input_file, output_file) in input_output_map {
		if input_file.extension() == Some(OsStr::new(util::XML_EXTENSION)) {
			if let Some(parent) = input_file.parent() {
				if parent == virtual_dir {
					continue;
				}
			}
		}
		let metadata = fs::metadata(input_file).unwrap_or_else(|e| {
			panic!(
				"Failed fetching metadata for {}: {}",
				input_file.display(),
				e
			)
		});

		let modified = metadata.modified().unwrap_or_else(|e| {
			panic!(
				"Failed fetching modified time for {}: {}",
				input_file.display(),
				e
			)
		});

		if modified > newest_time {
			newest_time = modified;
			newest_file = Some(output_file.clone());
		}
	}

	if let Some(file) = &newest_file {
		println!("Newest file: {}", &file.path.display());
	}

	newest_file.map(|p| p.path.clone())
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
	fs_cond: &Arc<(Mutex<Refresh>, Condvar)>,
	mut input_output_map: &mut HashMap<PathBuf, ComputedFilePath>,
) -> ! {
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

	loop {
		match rx.recv() {
			Ok(event) => {
				println!("Got {:?}", event);
				match event {
					notify::DebouncedEvent::Write(path)
					| notify::DebouncedEvent::Create(path) => {
						let path_to_communicate = get_path_to_refresh(
							&path,
							input_dir,
							output_dir,
							&mut input_output_map,
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
	input_file_path: &PathBuf,
	input_dir: &PathBuf,
	output_dir: &PathBuf,
	input_output_map: &mut HashMap<PathBuf, ComputedFilePath>,
) -> Option<PathBuf> {
	let css_extension = OsStr::new(util::CSS_EXTENSION);
	let html_extension = OsStr::new(util::HTML_EXTENSION);
	let markdown_extension = OsStr::new(util::MARKDOWN_EXTENSION);

	let canonical_input_path =
		input_file_path.canonicalize().unwrap_or_else(|e| {
			panic!(
				"Canonicalization of {} failed: {}",
				input_file_path.display(),
				e
			)
		});
	if canonical_input_path.extension() == Some(markdown_extension) {
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

		return Some(
			markdown::process_file(
				&canonical_input_path,
				input_dir,
				output_dir,
				input_output_map,
			)
			.path,
		);
	} else if canonical_input_path.extension() == Some(html_extension) {
		let parent_path = canonical_input_path.parent().unwrap_or_else(|| {
			panic!(
				"Path without a parent directory?: {}",
				canonical_input_path.display()
			)
		});
		let parent_path_file_name =
			parent_path.file_name().unwrap_or_else(|| {
				panic!("Missing file name in path: {}", parent_path.display())
			});
		if parent_path_file_name == "_layouts" {
			let template_file_stem =
				canonical_input_path.file_stem().unwrap_or_else(|| {
					panic!(
						"Missing file stem in path: {}",
						canonical_input_path.display()
					)
				});
			let mut dir_name = OsString::from(template_file_stem);
			dir_name.push("s");
			let markdown_dir = input_dir.join(dir_name);
			// If for example the post.html template was changed, try to get all
			// markdown files under /posts/.
			let files = if markdown_dir.exists() {
				markdown::get_files(&markdown_dir)
			} else {
				markdown::InputFileCollection::new()
			};

			// If we didn't find any markdown files, assume that the template
			// file just exists for the sake of a single markdown file.
			if files.is_empty() {
				let templated_file = input_dir
					.join(template_file_stem)
					.with_extension(markdown_extension);
				if templated_file.exists() {
					return Some(
						markdown::process_file(
							&templated_file,
							input_dir,
							output_dir,
							input_output_map,
						)
						.path,
					);
				}
			} else {
				let mut output_files = Vec::new();
				for file_name in &files.markdown {
					output_files.push(markdown::process_file(
						file_name,
						input_dir,
						output_dir,
						input_output_map,
					))
				}

				return output_files.first().map(|g| g.path.clone());
			}
		} else if parent_path_file_name == "_includes" {
			// Since we don't track what includes what, just do a full refresh.
			let files = markdown::get_files(input_dir);
			for file_name in &files.markdown {
				markdown::process_file(
					file_name,
					input_dir,
					output_dir,
					input_output_map,
				);
			}

			// Special identifier making JavaScript reload the current page.
			return Some(PathBuf::from("*"));
		} else {
			return Some(markdown::process_template_file_without_markdown(
				&canonical_input_path,
				input_dir,
				output_dir,
				input_output_map,
			));
		}
	} else if canonical_input_path.extension() == Some(css_extension) {
		util::copy_files_with_prefix(
			&[canonical_input_path],
			input_dir,
			output_dir,
		);
		// TODO: input_output_map
		// Special identifier making JavaScript reload the current page.
		return Some(PathBuf::from("*"));
	}

	None
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
	const TEXT_OUTPUT_EXTENSIONS: [&str; 3] = [
		util::HTML_EXTENSION,
		util::CSS_EXTENSION,
		util::XML_EXTENSION,
	];
	if path.to_string_lossy() == "dev" {
		println!("Requested path is not a file, returning index.");
		let iframe_src = if let Some(path) = start_file {
			let mut s = String::from(" src=\"");
			s.push_str(&path.to_string_lossy());
			s.push_str("\"");
			s
		} else {
			String::from("")
		};

		write_to_stream_log_count(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=UTF-8\r\n\r\n<html>
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
	} else {
		let mut full_path = root_dir.join(&path);
		if !full_path.is_file() {
			let with_index = full_path.join("index.html");
			if with_index.is_file() {
				full_path = with_index;
			}
		}
		println!("Attempting to open: {}", full_path.display());
		match fs::File::open(&full_path) {
			Ok(mut input_file) => {
				if let Some(extension) = full_path.extension() {
					let extension = extension.to_string_lossy();
					if TEXT_OUTPUT_EXTENSIONS.iter().any(|&ext| ext == extension) {
						write_to_stream_log_count(format!("HTTP/1.1 200 OK\r\nContent-Type: text/{}; charset=UTF-8\r\n\r\n", extension).as_bytes(), &mut stream);
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

							write_to_stream_log_count(&buf[0..size], &mut stream);
						}
					} else {
						write_to_stream_log_count(
							format!("HTTP/1.1 500 Error\r\nContent-Type: text/html; charset=UTF-8\r\n\r\n<html><body>Unrecognized extension: {}</body></html>\r\n", full_path.display()).as_bytes(),
							&mut stream,
						)
					}
				} else {
						write_to_stream_log_count(
							format!("HTTP/1.1 500 Error\r\nContent-Type: text/html; charset=UTF-8\r\n\r\n<html><body>Missing extension: {}</body></html>\r\n", full_path.display()).as_bytes(),
							&mut stream,
						)
				}
			}
			Err(e) => {
				match e.kind() {
					ErrorKind::NotFound => write_to_stream_log_count(
						format!("HTTP/1.1 404 Not found\r\nContent-Type: text/html; charset=UTF-8\r\n\r\n<html><body>Couldn't find: {}</body></html>\r\n", full_path.display()).as_bytes(),
						&mut stream,
					),
					_ => write_to_stream_log_count(
						format!("HTTP/1.1 500 Error\r\n{}", e)
							.as_bytes(),
						&mut stream,
					)
				}
			}
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
				websocket::handle_stream(stream, &key, fs_cond)
			}
		}
	}
}
