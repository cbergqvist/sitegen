use std::collections::{hash_map::Entry, BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{mpsc::channel, Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;
use std::{env, fs};

use notify::{watcher, RecursiveMode, Watcher};

mod atom;
mod config;
mod front_matter;
mod liquid;
mod markdown;
mod util;
mod websocket;

#[cfg(test)]
mod tests;

use markdown::{GroupedOutputFile, InputFile, OptionOutputFile};
use util::{strip_prefix, write_to_stream, write_to_stream_log_count, Refresh};

enum ReadResult {
	GetRequest(PathBuf),
	WebSocket(String),
}

fn main() {
	let mut args = config::Args::new();
	args.parse(env::args());

	if args.help.value {
		println!(
			"SiteGen version 0.1
Christopher Bergqvist <chris@digitalpoetry.se>

Basic static site generator.

Arguments:"
		);
		args.print_help();

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

fn inner_main(config: &config::Config) {
	let input_files = markdown::get_files(&config.input_dir);
	let mut input_output_map;
	let mut groups;

	if input_files.is_empty() {
		println!(
			"Found no valid file entries under \"{}\".",
			config.input_dir.display()
		);
		input_output_map = HashMap::new();
		groups = HashMap::new();
	} else {
		fs::create_dir(&config.output_dir).unwrap_or_else(|e| {
			panic!(
				"Failed creating \"{}\": {}.",
				config.output_dir.display(),
				e
			)
		});

		let fs = build_initial_fileset(
			&input_files,
			&config.input_dir,
			&config.output_dir,
		);
		input_output_map = fs.input_output_map;
		groups = fs.groups;

		let feed_groups = process_initial_files(
			&input_files,
			&config.input_dir,
			&config.output_dir,
			&input_output_map,
			&groups,
			&fs.tags,
		);

		atom::generate(
			&feed_groups,
			&config.output_dir,
			&config.base_url,
			&config.author,
			&config.email,
		);

		util::copy_files_with_prefix(
			&input_files.raw,
			&config.input_dir,
			&config.output_dir,
		);

		let sitemap_url = write_sitemap_xml(
			&config.output_dir,
			&config.base_url,
			&input_output_map,
		);
		write_robots_txt(&config.output_dir, &sitemap_url);
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
	let start_file = find_newest_file(
		&input_output_map,
		&config.input_dir,
		&config.output_dir,
	);

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
		&mut groups,
	);
}

struct InitialFileSet {
	input_output_map: HashMap<PathBuf, OptionOutputFile>,
	groups: HashMap<String, Vec<InputFile>>,
	tags: HashMap<String, Vec<InputFile>>,
}

fn build_initial_fileset(
	input_files: &markdown::InputFileCollection,
	input_dir: &PathBuf,
	output_dir: &PathBuf,
) -> InitialFileSet {
	let mut result = InitialFileSet {
		input_output_map: HashMap::new(),
		groups: HashMap::new(),
		tags: HashMap::new(),
	};

	// First, build up the input -> output map so that later when we do
	// actual processing of files, we have records of all other files.
	// This allows us to properly detect broken links.
	for file_name in &input_files.html {
		let output_file = markdown::parse_fm_and_compute_output_path(
			file_name, input_dir, output_dir,
		);
		checked_insert(
			file_name,
			GroupedOutputFile {
				file: output_file.file.convert_to_option(),
				group: output_file.group,
			},
			&mut result.input_output_map,
			Some(&mut result.groups),
			Some(&mut result.tags),
		)
	}
	for file_name in &input_files.markdown {
		let output_file = markdown::parse_fm_and_compute_output_path(
			file_name, input_dir, output_dir,
		);
		checked_insert(
			file_name,
			GroupedOutputFile {
				file: output_file.file.convert_to_option(),
				group: output_file.group,
			},
			&mut result.input_output_map,
			Some(&mut result.groups),
			Some(&mut result.tags),
		)
	}
	for file_name in &input_files.raw {
		let output_file_path =
			output_dir.join(strip_prefix(file_name, input_dir));
		checked_insert(
			file_name,
			GroupedOutputFile {
				file: OptionOutputFile {
					path: output_file_path,
					front_matter: None,
				},
				group: None,
			},
			&mut result.input_output_map,
			Some(&mut result.groups),
			Some(&mut result.tags),
		)
	}

	// Use stable sort in attempt to stay relatively deterministic, even
	// though we are still relying on the file system to give us files with
	// exactly equal front matter dates in the same order.
	for entries in result.groups.values_mut() {
		entries.sort_by(|lhs, rhs| {
			rhs.front_matter.date.cmp(&lhs.front_matter.date)
		})
	}
	for entries in result.tags.values_mut() {
		entries.sort_by(|lhs, rhs| {
			rhs.front_matter.date.cmp(&lhs.front_matter.date)
		})
	}

	for group in result.groups.keys() {
		let xml_file = PathBuf::from("feeds")
			.join(group)
			.with_extension(util::XML_EXTENSION);
		checked_insert(
			&input_dir.join(&xml_file), // virtual input
			GroupedOutputFile {
				file: OptionOutputFile {
					path: output_dir.join(xml_file),
					front_matter: None,
				},
				group: None,
			},
			&mut result.input_output_map,
			None,
			Some(&mut result.tags),
		)
	}

	for tag in result.tags.keys() {
		let tags_file = PathBuf::from("tags")
			.join(&tag)
			.with_extension(util::HTML_EXTENSION);
		checked_insert(
			&input_dir.join(&tags_file), // virtual input
			GroupedOutputFile {
				file: OptionOutputFile {
					path: output_dir.join(tags_file),
					front_matter: Some(front_matter::FrontMatter {
						title: format!("Tag: {}", tag),
						date: None,
						published: true,
						edited: None,
						categories: Vec::new(),
						tags: Vec::new(),
						layout: None,
						custom_attributes: BTreeMap::new(),
						end_position: 0,
					}),
				},
				group: None,
			},
			&mut result.input_output_map,
			Some(&mut result.groups),
			None,
		)
	}

	result
}

fn process_initial_files(
	input_files: &markdown::InputFileCollection,
	input_dir: &PathBuf,
	output_dir: &PathBuf,
	input_output_map: &HashMap<PathBuf, OptionOutputFile>,
	groups: &HashMap<String, Vec<InputFile>>,
	tags: &HashMap<String, Vec<InputFile>>,
) -> HashMap<String, Vec<atom::FeedEntry>> {
	let mut feed_map = HashMap::new();

	for file_name in &input_files.html {
		markdown::process_template_file(
			file_name,
			input_dir,
			output_dir,
			input_output_map,
			groups,
		);
	}

	for file_name in &input_files.markdown {
		let generated = markdown::process_file(
			file_name,
			input_dir,
			output_dir,
			input_output_map,
			groups,
		);
		if let Some(group) = generated.group {
			let entry = atom::FeedEntry {
				front_matter: generated.file.front_matter,
				html_content: generated.html_content,
				permalink: generated.file.path,
			};
			match feed_map.entry(group.clone()) {
				Entry::Vacant(ve) => {
					ve.insert(vec![entry]);
				}
				Entry::Occupied(oe) => oe.into_mut().push(entry),
			}
		}
	}

	for (tag, entries) in tags {
		let tags_file = PathBuf::from("tags")
			.join(&tag)
			.with_extension(util::HTML_EXTENSION);

		markdown::generate_tag_file(
			&input_dir.join(tags_file),
			entries,
			input_dir,
			output_dir,
			input_output_map,
			groups,
		)
	}

	feed_map
}

fn checked_insert(
	key: &PathBuf,
	value: GroupedOutputFile,
	path_map: &mut HashMap<PathBuf, OptionOutputFile>,
	group_map: Option<&mut HashMap<String, Vec<InputFile>>>,
	tags_map: Option<&mut HashMap<String, Vec<InputFile>>>,
) {
	match path_map.entry(key.clone()) {
		Entry::Occupied(oe) => {
			panic!(
				"Key {} already had value: {}, when trying to insert: {}",
				oe.key().display(),
				oe.get().path.display(),
				value.file.path.display()
			);
		}
		Entry::Vacant(ve) => {
			let extension = ve.key().extension().map(OsStr::to_os_string);
			ve.insert(value.file.clone());

			if extension.as_deref()
				!= Some(OsStr::new(util::MARKDOWN_EXTENSION))
			{
				return;
			}

			let path = value.file.path;
			let front_matter = &value.file.front_matter
						.unwrap_or_else(|| panic!("Expect front matter for grouped files, but didn't get one for {}.", path.display()));
			if let Some(group_map) = group_map {
				if let Some(group) = value.group {
					let file = InputFile {
						front_matter: front_matter.clone(),
						path: key.clone(),
					};
					match group_map.entry(group) {
						Entry::Vacant(ve) => {
							ve.insert(vec![file]);
						}
						Entry::Occupied(oe) => oe.into_mut().push(file),
					}
				}
			}

			if let Some(tags_map) = tags_map {
				for tag in &front_matter.tags {
					let file = InputFile {
						front_matter: front_matter.clone(),
						path: key.clone(),
					};
					match tags_map.entry(tag.clone()) {
						Entry::Vacant(ve) => {
							ve.insert(vec![file]);
						}
						Entry::Occupied(oe) => oe.into_mut().push(file),
					}
				}
			}
		}
	};
}

fn find_newest_file(
	input_output_map: &HashMap<PathBuf, OptionOutputFile>,
	input_dir: &PathBuf,
	output_dir: &PathBuf,
) -> Option<PathBuf> {
	let mut newest_file = None;
	let mut newest_time = std::time::UNIX_EPOCH;

	let supported_extensions = [
		OsStr::new(util::HTML_EXTENSION),
		OsStr::new(util::MARKDOWN_EXTENSION),
	];

	let excluded_folder = input_dir.join("tags");

	for (input_file, output_file) in input_output_map {
		let extension = if let Some(e) = input_file.extension() {
			e
		} else {
			continue;
		};

		if !supported_extensions.iter().any(|e| e == &extension)
			|| input_file.starts_with(&excluded_folder)
		{
			continue;
		}

		let unique_path = strip_prefix(input_file, input_dir);
		if unique_path.starts_with("_") || unique_path.starts_with(".") {
			continue;
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
			newest_file = Some(&output_file.path);
		}
	}

	if let Some(file) = &newest_file {
		println!("Newest file: {}", &file.display());
	}

	newest_file.map(|p| strip_prefix(p, output_dir))
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
	println!("Listening for connections on http://{}:{}/dev", host, port);

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
	mut input_output_map: &mut HashMap<PathBuf, OptionOutputFile>,
	groups: &mut HashMap<String, Vec<InputFile>>,
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
							&make_relative(&path, input_dir),
							input_dir,
							output_dir,
							&mut input_output_map,
							groups,
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
	input_output_map: &mut HashMap<PathBuf, OptionOutputFile>,
	groups: &mut HashMap<String, Vec<InputFile>>,
) -> Option<String> {
	let css_extension = OsStr::new(util::CSS_EXTENSION);
	let html_extension = OsStr::new(util::HTML_EXTENSION);
	let markdown_extension = OsStr::new(util::MARKDOWN_EXTENSION);

	fs::create_dir(&output_dir).unwrap_or_else(|e| {
		if e.kind() != ErrorKind::AlreadyExists {
			panic!("Failed creating \"{}\": {}.", output_dir.display(), e)
		}
	});

	if input_file_path.extension() == Some(markdown_extension) {
		let generated_file = markdown::reprocess_file(
			input_file_path,
			input_dir,
			output_dir,
			input_output_map,
			groups,
		);
		if let Some(group) = generated_file.group {
			let index_file = input_dir.join(group).join("index.html");
			if index_file.exists() {
				markdown::process_template_file(
					&index_file,
					input_dir,
					output_dir,
					input_output_map,
					groups,
				);
			}
		}
		Some(generated_file.file.path.to_string_lossy().to_string())
	} else if input_file_path.extension() == Some(html_extension) {
		handle_html_updated(
			input_file_path,
			input_dir,
			output_dir,
			input_output_map,
			groups,
		)
	} else if input_file_path.extension() == Some(css_extension) {
		util::copy_files_with_prefix(
			&[input_file_path.clone()],
			input_dir,
			output_dir,
		);

		match input_output_map.entry(input_file_path.clone()) {
			Entry::Occupied(..) => {}
			Entry::Vacant(ve) => {
				let output_file_path =
					output_dir.join(strip_prefix(input_file_path, input_dir));
				ve.insert(OptionOutputFile {
					path: output_file_path,
					front_matter: None,
				});
			}
		}

		Some(String::from(util::RELOAD_CURRENT))
	} else {
		None
	}
}

fn make_relative(input_file_path: &PathBuf, input_dir: &PathBuf) -> PathBuf {
	assert!(input_file_path.is_absolute());
	if input_dir.is_absolute() {
		panic!(
			"Don't currently handle absolute input dirs: {}",
			input_dir.display()
		);
	}

	let absolute_input_dir = input_dir.canonicalize().unwrap_or_else(|e| {
		panic!("Canonicalization of {} failed: {}", input_dir.display(), e)
	});

	input_dir.join(strip_prefix(input_file_path, &absolute_input_dir))
}

fn handle_html_updated(
	input_file_path: &PathBuf,
	input_dir: &PathBuf,
	output_dir: &PathBuf,
	input_output_map: &mut HashMap<PathBuf, OptionOutputFile>,
	groups: &mut HashMap<String, Vec<InputFile>>,
) -> Option<String> {
	let parent_path = input_file_path.parent().unwrap_or_else(|| {
		panic!(
			"Path without a parent directory?: {}",
			input_file_path.display()
		)
	});
	let parent_path_file_name = parent_path.file_name().unwrap_or_else(|| {
		panic!("Missing file name in path: {}", parent_path.display())
	});
	if parent_path_file_name == "_layouts" {
		let template_file_stem =
			input_file_path.file_stem().unwrap_or_else(|| {
				panic!(
					"Missing file stem in path: {}",
					input_file_path.display()
				)
			});
		let mut dir_name = OsString::from(template_file_stem);
		dir_name.push("s");
		let markdown_dir = input_dir.join(dir_name);
		// If for example the post.html template was changed, try to get all
		// markdown files under /posts/.
		let files_using_layout = if markdown_dir.exists() {
			markdown::get_files(&markdown_dir)
		} else {
			markdown::InputFileCollection::new()
		};

		// If we didn't find any markdown files, assume that the template
		// file just exists for the sake of a single markdown file.
		if files_using_layout.is_empty() {
			let templated_file = input_dir
				.join(template_file_stem)
				.with_extension(util::MARKDOWN_EXTENSION);
			if templated_file.exists() {
				Some(
					markdown::process_file(
						&templated_file,
						input_dir,
						output_dir,
						input_output_map,
						groups,
					)
					.file
					.path
					.to_string_lossy()
					.to_string(),
				)
			} else {
				None
			}
		} else {
			let mut output_files = Vec::new();
			for file_name in &files_using_layout.markdown {
				output_files.push(markdown::process_file(
					file_name,
					input_dir,
					output_dir,
					input_output_map,
					groups,
				))
			}

			output_files
				.first()
				.map(|g| g.file.path.to_string_lossy().to_string())
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
				groups,
			);
		}

		Some(String::from(util::RELOAD_CURRENT))
	} else {
		Some(
			markdown::reprocess_template_file(
				input_file_path,
				input_dir,
				output_dir,
				input_output_map,
				groups,
			)
			.to_string_lossy()
			.to_string(),
		)
	}
}

fn handle_read(stream: &mut TcpStream) -> Option<ReadResult> {
	let mut buf = [0_u8; 4096];
	let size = stream
		.read(&mut buf)
		.unwrap_or_else(|e| panic!("WARNING: Unable to read stream: {}", e));

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
	let first_line = lines
		.next()
		.unwrap_or_else(|| panic!("Missing lines in HTTP request."));
	let mut components = first_line.split(' ');
	let method = components.next().unwrap_or_else(|| {
		panic!(
			"Missing components in first HTTP request line: {}",
			first_line
		)
	});
	if method != "GET" {
		panic!("Unsupported method \"{}\", line: {}", method, first_line)
	}

	let path = components
		.next()
		.unwrap_or_else(|| panic!("Missing path in: {}", first_line));

	let mut websocket_key = None;
	for line in lines {
		let mut components = line.split(' ');
		if let Some(component) = components.next() {
			if component == "Sec-WebSocket-Key:" {
				websocket_key = components.next();
			} else if component == "Sec-WebSocket-Protocol:" {
				let protocols: String = components.collect::<String>();
				panic!("We don't handle protocols correctly yet: {}", protocols)
			}
		}
	}

	if let Some(key) = websocket_key {
		return Some(ReadResult::WebSocket(key.to_string()));
	}

	if !path.starts_with('/') {
		panic!(
			"Expected path to start with leading slash, but got: {}",
			path
		)
	}
	Some(ReadResult::GetRequest(PathBuf::from(
		// Strip leading root slash.
		&path[1..],
	)))
}

const DEV_PAGE_HEADER: &[u8; 1151] = b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=UTF-8\r\n\r\n<html>
<head><script>
// Tag on time in order to distinguish different sockets.
let socket = new WebSocket(\"ws://\" + window.location.hostname + \":\" + window.location.port + \"/chat?now=\" + Date.now())
socket.onopen = function(e) {
	//alert(\"[open] Connection established\")
}
socket.onmessage = function(e) {
	reader = new FileReader()
	reader.onload = () => {
		text = reader.result
		if (text == \"*\") {
			window.frames['preview'].location.reload()
		} else {
			window.frames['preview'].location.href = text
		}
	}
	reader.readAsText(e.data)
}
socket.onerror = function(e) {
	alert(`Socket error: ${e}`)
}
window.addEventListener('beforeunload', (event) => {
	socket.close()
});
</script>
<style type=\"text/css\">
BODY {
	font-family: \"Helvetica Neue\", Helvetica, Arial, sans-serif;
	margin: 0;
}
.banner {
	background: rgba(0, 0, 255, 0.4);
	position: fixed;
}
@media (prefers-color-scheme: dark) {
	BODY {
		background: black; /* Prevents white flash on Firefox. */
		color: white;
	}
}
</style>
</head>
<body>
<div class=\"banner\">Preview, save Markdown file to disk for live reload:</div>
";
const DEV_PAGE_FOOTER: &[u8; 17] = b"</body>
</html>\r\n";

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

		write_to_stream_log_count(DEV_PAGE_HEADER, &mut stream);
		write_to_stream_log_count(format!("<iframe name=\"preview\"{} style=\"border: 0; margin: 0; width: 100%; height: 100%\"></iframe>
", iframe_src).as_bytes(), &mut stream);
		write_to_stream_log_count(DEV_PAGE_FOOTER, &mut stream);
		return;
	}

	let mut full_path = root_dir.join(&path);
	if !full_path.is_file() {
		let with_index = full_path.join("index.html");
		if with_index.is_file() {
			full_path = with_index;
		}
	}

	println!("Attempting to open: {}", full_path.display());
	let mut input_file = match fs::File::open(&full_path) {
		Ok(input) => input,
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
			return;
		}
	};

	if let Some(extension) = full_path.extension() {
		let extension = extension.to_string_lossy();
		if TEXT_OUTPUT_EXTENSIONS.iter().any(|&ext| ext == extension) {
			write_to_stream_log_count(format!("HTTP/1.1 200 OK\r\nContent-Type: text/{}; charset=UTF-8\r\n\r\n", extension).as_bytes(), &mut stream);
			let mut buf = [0_u8; 64 * 1024];
			loop {
				let size = input_file.read(&mut buf).unwrap_or_else(|e| {
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

fn write_robots_txt(output_dir: &PathBuf, sitemap_url: &str) {
	let file_name = output_dir.join(PathBuf::from("robots.txt"));
	let mut file = fs::File::create(&file_name).unwrap_or_else(|e| {
		panic!("Failed creating {}: {}", file_name.display(), e)
	});
	file.write_all(
		format!(
			"User-agent: *
Allow: /
Sitemap: {}
",
			sitemap_url
		)
		.as_bytes(),
	)
	.unwrap_or_else(|e| {
		panic!("Failed writing to {}: {}", file_name.display(), e)
	});
	// Avoiding sync_all() for now to be friendlier to disks.
	file.sync_data().unwrap_or_else(|e| {
		panic!("Failed sync_data() for \"{}\": {}.", file_name.display(), e)
	});
	println!("Wrote {}.", file_name.display());
}

fn write_sitemap_xml(
	output_dir: &PathBuf,
	base_url: &str,
	input_output_map: &HashMap<PathBuf, OptionOutputFile>,
) -> String {
	let official_file_name = PathBuf::from("sitemap.xml");
	let file_name = output_dir.join(&official_file_name);
	let mut file = fs::File::create(&file_name).unwrap_or_else(|e| {
		panic!("Failed creating {}: {}", file_name.display(), e)
	});
	write_to_stream(
		b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">
",
		&mut file,
	);

	let html_extension = OsStr::new(util::HTML_EXTENSION);

	for output_file in input_output_map.values() {
		if output_file.path.extension() != Some(html_extension) {
			continue;
		}

		let path = strip_prefix(&output_file.path, output_dir);
		let mut output_url = base_url.to_string();
		if path.file_name() == Some(OsStr::new("index.html")) {
			output_url.push_str(&path.with_file_name("").to_string_lossy())
		} else {
			output_url.push_str(&path.to_string_lossy())
		}

		write_to_stream(
			format!(
				"	<url>
		<loc>{}</loc>
",
				output_url
			)
			.as_bytes(),
			&mut file,
		);

		if let Some(front_matter) = &output_file.front_matter {
			if let Some(date) = &front_matter.edited {
				write_to_stream(
					format!(
						"		<lastmod>{}</lastmod>
",
						date
					)
					.as_bytes(),
					&mut file,
				);
			} else if let Some(date) = &front_matter.date {
				write_to_stream(
					format!(
						"		<lastmod>{}</lastmod>
",
						date
					)
					.as_bytes(),
					&mut file,
				);
			}
		}

		write_to_stream(
			b"	</url>
",
			&mut file,
		);
	}

	write_to_stream(
		b"</urlset>
",
		&mut file,
	);
	// Avoiding sync_all() for now to be friendlier to disks.
	file.sync_data().unwrap_or_else(|e| {
		panic!("Failed sync_data() for \"{}\": {}.", file_name.display(), e)
	});
	println!("Wrote {}.", file_name.display());

	let mut result = base_url.to_string();
	result.push_str(&official_file_name.to_string_lossy());
	result
}
