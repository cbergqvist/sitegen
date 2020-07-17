use std::collections::{hash_map::Entry, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::{mpsc::channel, Arc, Condvar, Mutex};
use std::time::Duration;

use notify::{watcher, DebouncedEvent, RecursiveMode, Watcher};

use crate::config::{make_site_info, Config};
use crate::markdown;
use crate::markdown::{GroupedOptionOutputFile, InputFile, OptionOutputFile};
use crate::util;
use crate::util::{
	find_newest_file, get_front_matter_and_output_path, make_relative,
	translate_input_to_output, Refresh,
};

pub fn run(
	fs_cond: &Arc<(Mutex<Refresh>, Condvar)>,
	mut input_output_map: &mut HashMap<PathBuf, GroupedOptionOutputFile>,
	groups: &mut HashMap<String, Vec<InputFile>>,
	tags: &mut HashMap<String, Vec<InputFile>>,
	config: &Config,
) -> ! {
	let (tx, rx) = channel();
	let mut watcher = watcher(tx, Duration::from_millis(200))
		.unwrap_or_else(|e| panic!("Unable to create watcher: {}", e));

	watcher
		.watch(&config.input_dir, RecursiveMode::Recursive)
		.unwrap_or_else(|e| {
			panic!("Unable to watch {}: {}", config.input_dir.display(), e)
		});

	loop {
		let event = rx.recv().unwrap_or_else(|e| panic!("Watch error: {}", e));
		match event {
			DebouncedEvent::Write(path) | DebouncedEvent::Create(path) => {
				let relative_path = make_relative(&path, &config.input_dir);
				let path_to_communicate = get_path_to_refresh(
					&relative_path,
					&mut input_output_map,
					groups,
					tags,
					config,
				);
				println!(
					"Path to communicate in response to write/create of {}: {:?}",
					relative_path.display(), path_to_communicate
				);
				if path_to_communicate.is_some() {
					let (mutex, cvar) = &**fs_cond;

					let mut refresh = mutex.lock().unwrap_or_else(|e| {
						panic!("Failed locking mutex: {}", e)
					});
					refresh.file = path_to_communicate;
					refresh.index += 1;
					cvar.notify_all();
				}
			}
			DebouncedEvent::Rename(..) | DebouncedEvent::Remove(..) => {
				panic!("Detected {:?}, we don't support live-updating after such events.", event)
			}
			DebouncedEvent::NoticeWrite(..)
			| DebouncedEvent::NoticeRemove(..)
			| DebouncedEvent::Chmod(..) => println!("Skipping event: {:?}", event),
			DebouncedEvent::Rescan => {
				unimplemented!("Rescanning is not implemented")
			}
			DebouncedEvent::Error(e, path) => {
				panic!("notify encountered error: {}, path: {:?}", e, path)
			}
		}
	}
}

fn get_path_to_refresh(
	input_file_path: &PathBuf,
	input_output_map: &mut HashMap<PathBuf, GroupedOptionOutputFile>,
	groups: &mut HashMap<String, Vec<InputFile>>,
	tags: &mut HashMap<String, Vec<InputFile>>,
	config: &Config,
) -> Option<String> {
	let css_extension = OsStr::new(util::CSS_EXTENSION);
	let html_extension = OsStr::new(util::HTML_EXTENSION);
	let markdown_extension = OsStr::new(util::MARKDOWN_EXTENSION);

	fs::create_dir(&config.output_dir).unwrap_or_else(|e| {
		if e.kind() != ErrorKind::AlreadyExists {
			panic!(
				"Failed creating \"{}\": {}.",
				config.output_dir.display(),
				e
			)
		}
	});

	if input_file_path.extension() == Some(markdown_extension) {
		let grouped_file = markdown::parse_fm_and_compute_output_path(
			input_file_path,
			&config.input_dir,
			&config.output_dir,
		);
		let site_info = make_site_info(&config);
		markdown::reindex(
			input_file_path,
			&grouped_file,
			&config.input_dir,
			&config.output_dir,
			input_output_map,
			groups,
			tags,
			&site_info,
		);
		if !grouped_file.file.front_matter.published && !config.deploy {
			return None;
		}

		let generated_file = markdown::process_file(
			input_file_path,
			&grouped_file.file.path,
			&grouped_file.file.front_matter,
			&config.input_dir,
			&config.output_dir,
			input_output_map,
			groups,
			&site_info,
		);
		if let Some(group) = generated_file.group {
			let index_file = config.input_dir.join(group).join("index.html");
			if index_file.exists() {
				markdown::process_template_file(
					&index_file,
					&config.input_dir,
					&config.output_dir,
					input_output_map,
					groups,
					&site_info,
				);
			}
		}
		Some(generated_file.file.path.to_string_lossy().to_string())
	} else if input_file_path.extension() == Some(html_extension) {
		handle_html_updated(input_file_path, input_output_map, groups, config)
	} else if input_file_path.extension() == Some(css_extension) {
		util::copy_files_with_prefix(
			&[input_file_path.clone()],
			&config.input_dir,
			&config.output_dir,
		);

		match input_output_map.entry(input_file_path.clone()) {
			Entry::Occupied(..) => {}
			Entry::Vacant(ve) => {
				ve.insert(GroupedOptionOutputFile {
					file: OptionOutputFile {
						path: translate_input_to_output(
							input_file_path,
							&config.input_dir,
							&config.output_dir,
						),
						front_matter: None,
					},
					group: None,
				});
			}
		}

		Some(String::from(util::RELOAD_CURRENT))
	} else {
		None
	}
}

fn handle_html_updated(
	input_file_path: &PathBuf,
	input_output_map: &mut HashMap<PathBuf, GroupedOptionOutputFile>,
	groups: &mut HashMap<String, Vec<InputFile>>,
	config: &Config,
) -> Option<String> {
	let site_info = make_site_info(&config);
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
		let markdown_dir = config.input_dir.join(dir_name);
		// If for example the /_layouts/post.html template was changed, try to
		// get all markdown files under /posts/.
		let files_using_layout = if markdown_dir.exists() {
			if markdown_dir == config.input_dir {
				markdown::get_files(&markdown_dir)
			} else {
				markdown::get_subdir_files(&markdown_dir)
			}
		} else {
			markdown::InputFileCollection::new()
		};

		if files_using_layout.markdown.is_empty() {
			let templated_file = config
				.input_dir
				.join(template_file_stem)
				.with_extension(util::MARKDOWN_EXTENSION);
			println!(
				"Didn't find any markdown files under {}, checking if the \
				template file {} exists just for the sake of a single markdown \
				file at: {}",
				markdown_dir.display(),
				input_file_path.display(),
				templated_file.display(),
			);
			if templated_file.exists() {
				if let Some((front_matter, output_file_path)) =
					get_front_matter_and_output_path(
						&templated_file,
						input_output_map,
						config.deploy,
					) {
					Some(
						markdown::process_file(
							&templated_file,
							output_file_path,
							front_matter,
							&config.input_dir,
							&config.output_dir,
							input_output_map,
							groups,
							&site_info,
						)
						.file
						.path
						.to_string_lossy()
						.to_string(),
					)
				} else {
					println!(
						"Skipping unpublished file: {}",
						templated_file.display()
					);
					None
				}
			} else {
				None
			}
		} else {
			println!(
				"Found {} files using layout {}.",
				files_using_layout.markdown.len(),
				input_file_path.display(),
			);
			let mut processed_files = HashMap::new();
			for file_name in &files_using_layout.markdown {
				if let Some((front_matter, output_file_path)) =
					get_front_matter_and_output_path(
						file_name,
						input_output_map,
						config.deploy,
					) {
					processed_files.insert(
						file_name.clone(),
						markdown::process_file(
							file_name,
							output_file_path,
							front_matter,
							&config.input_dir,
							&config.output_dir,
							input_output_map,
							groups,
							&site_info,
						),
					);
				} else {
					println!(
						"Skipping unpublished file: {}",
						file_name.display()
					)
				}
			}

			find_newest_file(&processed_files, &config.input_dir)
				.map(|g| g.file.path.to_string_lossy().to_string())
		}
	} else if parent_path_file_name == "_includes" {
		// Since we don't track what includes what, just do a full refresh.
		let files = markdown::get_files(&config.input_dir);
		for file_name in &files.markdown {
			if let Some((front_matter, output_file_path)) =
				get_front_matter_and_output_path(
					file_name,
					input_output_map,
					config.deploy,
				) {
				markdown::process_file(
					file_name,
					output_file_path,
					front_matter,
					&config.input_dir,
					&config.output_dir,
					input_output_map,
					groups,
					&site_info,
				);
			} else {
				println!("Skipping unpublished file: {}", file_name.display())
			}
		}

		Some(String::from(util::RELOAD_CURRENT))
	} else {
		Some(
			markdown::reprocess_template_file(
				input_file_path,
				&config.input_dir,
				&config.output_dir,
				input_output_map,
				groups,
				&site_info,
			)
			.to_string_lossy()
			.to_string(),
		)
	}
}
