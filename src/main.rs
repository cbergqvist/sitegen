use std::collections::{hash_map::Entry, BTreeMap, HashMap};
use std::ffi::OsStr;
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};
use std::{env, fs};

mod atom;
mod config;
mod front_matter;
mod http;
mod liquid;
mod markdown;
mod robots;
mod util;
mod watch_fs;
mod websocket;

#[cfg(test)]
mod tests;

use config::{make_site_info, Config};
use markdown::{GroupedOptionOutputFile, InputFile, OptionOutputFile};
use util::{
	find_newest_file, get_front_matter_and_output_path, strip_prefix,
	translate_input_to_output, Refresh,
};

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

	inner_main(&args.values())
}

fn inner_main(config: &Config) {
	let mut input_files = markdown::get_files(&config.input_dir);
	let mut input_output_map;
	let mut groups;
	let mut tags;

	if input_files.is_empty() {
		println!(
			"Found no valid file entries under \"{}\".",
			config.input_dir.display()
		);
		input_output_map = HashMap::new();
		groups = HashMap::new();
		tags = HashMap::new();
	} else {
		fs::create_dir(&config.output_dir).unwrap_or_else(|e| {
			panic!(
				"Failed creating \"{}\": {}.",
				config.output_dir.display(),
				e
			)
		});

		let fs = build_initial_fileset(
			&mut input_files,
			&config.input_dir,
			&config.output_dir,
			config.deploy,
		);
		input_output_map = fs.input_output_map;
		groups = fs.groups;
		tags = fs.tags;

		process_initial_files(
			&input_files,
			config,
			&input_output_map,
			&groups,
			&tags,
		)
	}

	if !config.watch && !config.deploy {
		return;
	}

	let fs_cond = Arc::new((
		Mutex::new(Refresh {
			index: 0,
			file: None,
		}),
		Condvar::new(),
	));
	let start_file = find_newest_file(&input_output_map, &config.input_dir)
		.map(|grouped_file| {
			let path = PathBuf::from("./").join(strip_prefix(
				&grouped_file.file.path,
				&config.output_dir,
			));
			if path.file_name() == Some(OsStr::new("index.html")) {
				path.with_file_name("")
			} else {
				path
			}
		});

	http::spawn_listening_thread(
		&config.host,
		config.port,
		PathBuf::from(&config.output_dir),
		if config.deploy {
			None
		} else {
			Some(fs_cond.clone())
		},
		start_file,
	);

	if config.deploy {
		println!(
			"NOT watching file system for changes since we are in deploy mode. \
			Just serving HTTP requests and wait for Ctrl+C on main thread."
		);
		loop {
			thread::sleep(Duration::from_millis(500))
		}
	} else {
		assert!(config.watch);

		// As we start watching some time after we've done initial processing, it is
		// possible that files get modified in between and changes get lost.
		watch_fs::run(
			&fs_cond,
			&mut input_output_map,
			&mut groups,
			&mut tags,
			config,
		);
	}
}

struct InitialFileSet {
	input_output_map: HashMap<PathBuf, GroupedOptionOutputFile>,
	groups: HashMap<String, Vec<InputFile>>,
	tags: HashMap<String, Vec<InputFile>>,
}

fn build_initial_fileset(
	input_files: &mut markdown::InputFileCollection,
	input_dir: &PathBuf,
	output_dir: &PathBuf,
	deploying: bool,
) -> InitialFileSet {
	let mut result = InitialFileSet {
		input_output_map: HashMap::new(),
		groups: HashMap::new(),
		tags: HashMap::new(),
	};

	// First, build up the input -> output map so that later when we do
	// actual processing of files, we have records of all other files.
	// This allows us to properly detect broken links.
	let mut unpublished = Vec::new();
	for file_name in &input_files.html {
		let output_file = markdown::parse_fm_and_compute_output_path(
			file_name, input_dir, output_dir,
		);
		if deploying && !output_file.file.front_matter.published {
			unpublished.push(file_name.clone());
			continue;
		}
		checked_insert(
			file_name,
			GroupedOptionOutputFile {
				file: output_file.file.convert_to_option(),
				group: output_file.group,
			},
			&mut result.input_output_map,
			Some(&mut result.groups),
			Some(&mut result.tags),
		)
	}
	input_files.html.retain(|f| !unpublished.contains(f));

	let mut unpublished = Vec::new();
	for file_name in &input_files.markdown {
		let output_file = markdown::parse_fm_and_compute_output_path(
			file_name, input_dir, output_dir,
		);
		if deploying && !output_file.file.front_matter.published {
			unpublished.push(file_name.clone());
			continue;
		}
		checked_insert(
			file_name,
			GroupedOptionOutputFile {
				file: output_file.file.convert_to_option(),
				group: output_file.group,
			},
			&mut result.input_output_map,
			Some(&mut result.groups),
			Some(&mut result.tags),
		)
	}
	input_files.markdown.retain(|f| !unpublished.contains(f));

	for file_name in &input_files.raw {
		checked_insert(
			file_name,
			GroupedOptionOutputFile {
				file: OptionOutputFile {
					path: translate_input_to_output(
						file_name, input_dir, output_dir,
					),
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
			GroupedOptionOutputFile {
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
			.join(tag)
			.with_extension(util::HTML_EXTENSION);
		checked_insert(
			&input_dir.join(&tags_file), // virtual input
			GroupedOptionOutputFile {
				file: OptionOutputFile {
					path: output_dir.join(tags_file),
					front_matter: Some(Arc::new(front_matter::FrontMatter {
						title: format!("Tag: {}", tag),
						date: None,
						published: true,
						edited: None,
						categories: Vec::new(),
						tags: Vec::new(),
						layout: None,
						custom_attributes: BTreeMap::new(),
						end_position: 0,
						subsequent_line: 1,
					})),
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
	config: &Config,
	input_output_map: &HashMap<PathBuf, GroupedOptionOutputFile>,
	groups: &HashMap<String, Vec<InputFile>>,
	tags: &HashMap<String, Vec<InputFile>>,
) {
	let timer = Instant::now();

	let mut file_count = 0;
	crossbeam_utils::thread::scope(|s| {
		let feed_map = Arc::new(RwLock::new(HashMap::new()));
		let mut feed_map_writers = Vec::new();
		let mut processed_single = false;
		for file_name in &input_files.markdown {
			if config.single_file.is_some()
				&& config.single_file.as_deref() != Some(file_name)
			{
				continue;
			}

			processed_single = true;
			let feed_map_c = feed_map.clone();
			let handle = s.spawn(move |_| {
				if let Some((front_matter, output_file_path)) =
					get_front_matter_and_output_path(
						file_name,
						input_output_map,
						config.deploy,
					) {
					// TODO: Understand Rust better so I don't have to create
					// new copies of SiteInfo all the time.
					let generated = markdown::process_file(
						file_name,
						output_file_path,
						front_matter,
						&config.input_dir,
						&config.output_dir,
						input_output_map,
						groups,
						&make_site_info(config),
					);
					if let Some(group) = generated.group {
						let entry = atom::FeedEntry {
							front_matter: generated.file.front_matter,
							html_content: generated.html_content,
							permalink: generated.file.path,
						};
						let mut locked_feed_map =
							feed_map_c.write().unwrap_or_else(|e| {
								panic!(
									"Failed acquiring feed map write-lock: {}",
									e
								)
							});
						match locked_feed_map.entry(group) {
							Entry::Vacant(ve) => {
								ve.insert(vec![entry]);
							}
							Entry::Occupied(oe) => oe.into_mut().push(entry),
						}
					}
				} else {
					println!(
						"Skipping unpublished file: {}",
						file_name.display()
					)
				}
			});
			if config.serial {
				handle.join().unwrap_or_else(|e| {
					panic!("Failed joining on thread: {:?}", e)
				});
			} else {
				feed_map_writers.push(handle);
			}
		}
		file_count += input_files.markdown.len();

		for file_name in &input_files.html {
			if config.single_file.is_some()
				&& config.single_file.as_deref() != Some(file_name)
			{
				continue;
			}

			processed_single = true;
			let handle = s.spawn(move |_| {
				if let Some((front_matter, output_file_path)) =
					get_front_matter_and_output_path(
						file_name,
						input_output_map,
						config.deploy,
					) {
					markdown::process_template_file(
						file_name,
						output_file_path,
						front_matter,
						&config.input_dir,
						&config.output_dir,
						input_output_map,
						groups,
						&make_site_info(config),
					)
				}
			});
			if config.serial {
				handle.join().unwrap_or_else(|e| {
					panic!("Failed joining on thread: {:?}", e)
				});
			}
		}
		file_count += input_files.html.len();

		let raw = if let Some(single_file) = &config.single_file {
			if input_files.raw.contains(single_file) {
				processed_single = true;
				vec![single_file.clone()]
			} else {
				Vec::new()
			}
		} else {
			input_files.raw.clone()
		};

		let handle = s.spawn(move |_| {
			util::copy_files_with_prefix(
				&raw,
				&config.input_dir,
				&config.output_dir,
			);
		});
		if config.serial {
			handle.join().unwrap_or_else(|e| {
				panic!("Failed joining on thread: {:?}", e)
			});
		}
		file_count += input_files.raw.len();

		for (tag, entries) in tags {
			let tags_file = PathBuf::from("tags")
				.join(tag)
				.with_extension(util::HTML_EXTENSION);
			if config.single_file.is_some()
				&& config.single_file.as_deref() != Some(&tags_file)
			{
				continue;
			}

			processed_single = true;
			let handle = s.spawn(move |_| {
				markdown::generate_tag_file(
					&config.input_dir.join(tags_file),
					entries,
					&config.input_dir,
					&config.output_dir,
					input_output_map,
					groups,
					&make_site_info(config),
				);
			});
			if config.serial {
				handle.join().unwrap_or_else(|e| {
					panic!("Failed joining on thread: {:?}", e)
				});
			}
		}
		file_count += tags.len();

		if let Some(single_file) = &config.single_file {
			if !processed_single {
				panic!("Failed finding single file: {}", single_file.display());
			}
		}

		let handle = s.spawn(|_| {
			let sitemap_url = robots::write_sitemap_xml(
				&config.output_dir,
				&config.base_url,
				input_output_map,
			);
			robots::write_robots_txt(&config.output_dir, &sitemap_url);
		});
		if config.serial {
			handle.join().unwrap_or_else(|e| {
				panic!("Failed joining on thread: {:?}", e)
			});
		}
		file_count += 2;

		for handle in feed_map_writers {
			handle.join().unwrap_or_else(|e| {
				panic!("Failed joining on thread: {:?}", e)
			});
		}
		atom::generate(
			Arc::try_unwrap(feed_map)
				.unwrap_or_else(|_arc: Arc<_>| panic!("Failed unwrapping Arc"))
				.into_inner()
				.unwrap_or_else(|e| {
					panic!("Failed acquiring feed map read-lock: {}", e)
				}),
			&config.output_dir,
			&config.base_url,
			&config.author,
			&config.email,
			&config.title,
		);
		file_count += 1;
	})
	.unwrap_or_else(|e| panic!("Crossbeam scope failed: {:?}", e));

	println!(
		"Processed {} files in {} ms.",
		file_count,
		timer.elapsed().as_millis()
	)
}

fn checked_insert(
	key: &Path,
	value: GroupedOptionOutputFile,
	path_map: &mut HashMap<PathBuf, GroupedOptionOutputFile>,
	group_map: Option<&mut HashMap<String, Vec<InputFile>>>,
	tags_map: Option<&mut HashMap<String, Vec<InputFile>>>,
) {
	match path_map.entry(key.to_path_buf()) {
		Entry::Occupied(oe) => {
			panic!(
				"Key {} already had value: {}, when trying to insert: {}",
				oe.key().display(),
				oe.get().file.path.display(),
				value.file.path.display()
			);
		}
		Entry::Vacant(ve) => {
			let extension = ve.key().extension().map(OsStr::to_os_string);
			ve.insert(value.clone());

			if extension.as_deref()
				!= Some(OsStr::new(util::MARKDOWN_EXTENSION))
			{
				return;
			}

			let front_matter = value.file.front_matter
				.unwrap_or_else(|| panic!("Expect front matter for grouped files, but didn't get one for {}.", key.display()));
			let file = InputFile {
				front_matter,
				path: key.to_path_buf(),
			};

			if let Some(tags_map) = tags_map {
				for tag in &file.front_matter.tags {
					match tags_map.entry(tag.clone()) {
						Entry::Vacant(ve) => {
							ve.insert(vec![file.clone()]);
						}
						Entry::Occupied(oe) => oe.into_mut().push(file.clone()),
					}
				}
			}

			if let Some(group_map) = group_map {
				if let Some(group) = value.group {
					match group_map.entry(group) {
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
