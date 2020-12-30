use std::collections::{hash_map::Entry, BTreeMap, HashMap};
use std::ffi::OsStr;
use std::fs;
use std::io::{BufReader, BufWriter, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use pulldown_cmark::{html, Parser, Options};

use crate::front_matter::FrontMatter;
use crate::liquid;
use crate::util;
use crate::util::{strip_prefix, SiteInfo};

#[derive(Clone)]
pub struct GroupedOptionOutputFile {
	pub file: OptionOutputFile,
	pub group: Option<String>,
}

pub struct ComputedTemplatePath {
	pub path: PathBuf,
	pub group: Option<String>,
}

pub struct GroupedOutputFile {
	pub file: OutputFile,
	pub group: Option<String>,
}

pub struct GeneratedFile {
	pub file: OutputFile,
	pub group: Option<String>,
	pub html_content: String,
}

#[derive(Clone)]
pub struct OutputFile {
	pub front_matter: Arc<FrontMatter>,
	pub path: PathBuf,
}

#[derive(Clone)]
pub struct InputFile {
	pub front_matter: Arc<FrontMatter>,
	pub path: PathBuf,
}

#[derive(Clone)]
pub struct OptionOutputFile {
	pub front_matter: Option<Arc<FrontMatter>>,
	pub path: PathBuf,
}

impl OutputFile {
	pub fn convert_to_option(self) -> OptionOutputFile {
		OptionOutputFile {
			front_matter: Some(self.front_matter),
			path: self.path,
		}
	}
}

impl GroupedOutputFile {
	pub fn clone_to_option(&self) -> GroupedOptionOutputFile {
		GroupedOptionOutputFile {
			file: OptionOutputFile {
				front_matter: Some(self.file.front_matter.clone()),
				path: self.file.path.clone(),
			},
			group: self.group.clone(),
		}
	}
}

pub struct InputFileCollection {
	pub html: Vec<PathBuf>,
	pub markdown: Vec<PathBuf>,
	pub raw: Vec<PathBuf>,
}

impl InputFileCollection {
	pub const fn new() -> Self {
		Self {
			html: Vec::new(),
			markdown: Vec::new(),
			raw: Vec::new(),
		}
	}

	pub fn is_empty(&self) -> bool {
		self.html.is_empty() && self.markdown.is_empty() && self.raw.is_empty()
	}

	fn append(&mut self, other: &mut Self) {
		self.html.append(&mut other.html);
		self.markdown.append(&mut other.markdown);
		self.raw.append(&mut other.raw);
	}
}

enum Level {
	Root,
	SubDir,
	SubDirStatic,
}

pub fn get_files(input_dir: &PathBuf) -> InputFileCollection {
	get_files_recursively(input_dir, &Level::Root)
}

pub fn get_subdir_files(input_dir: &PathBuf) -> InputFileCollection {
	get_files_recursively(input_dir, &Level::SubDir)
}

fn get_files_recursively(
	input_dir: &PathBuf,
	level: &Level,
) -> InputFileCollection {
	let css_extension = OsStr::new(util::CSS_EXTENSION);
	let html_extension = OsStr::new(util::HTML_EXTENSION);
	let markdown_extension = OsStr::new(util::MARKDOWN_EXTENSION);

	let entries = fs::read_dir(input_dir).unwrap_or_else(|e| {
		panic!(
			"Failed reading paths from \"{}\": {}.",
			input_dir.display(),
			e
		)
	});
	let mut result = InputFileCollection::new();
	for entry in entries {
		let entry = entry.unwrap_or_else(|e| {
			panic!("Invalid entry in \"{}\": {}", input_dir.display(), e)
		});

		let path = entry.path();
		let ft = entry.file_type().unwrap_or_else(|e| {
			panic!(
				"Failed getting file type of {}: {}",
				entry.path().display(),
				e
			)
		});

		if ft.is_file() {
			match level {
				Level::SubDirStatic => {
					println!(
						"Adding file from static directory: {}",
						path.display()
					);
					result.raw.push(path)
				}
				Level::Root | Level::SubDir => {
					if let Some(extension) = path.extension() {
						let recognized = || {
							println!(
								"File with recognized extension: \"{}\"",
								entry.path().display()
							)
						};
						if extension == html_extension {
							result.html.push(path);
							recognized();
						} else if extension == markdown_extension {
							result.markdown.push(path);
							recognized();
						} else if extension == css_extension {
							result.raw.push(path);
							recognized();
						} else {
							println!(
								"Skipping file with unrecognized extension ({}) file: \"{}\"",
								extension.to_string_lossy(),
								entry.path().display()
							);
						}
					} else {
						println!(
							"Skipping extension-less file: \"{}\"",
							entry.path().display()
						);
					}
				}
			}
		} else if ft.is_dir() {
			match level {
				Level::SubDirStatic => {
					let mut subdir_files = self::get_files_recursively(
						&path,
						&Level::SubDirStatic,
					);
					result.append(&mut subdir_files);
				}
				Level::Root | Level::SubDir => {
					let file_name = path
						.file_name()
						.unwrap_or_else(|| {
							panic!(
								"Directory without filename?: {}",
								path.display()
							)
						})
						.to_string_lossy();

					if file_name == "_static" {
						if let Level::Root = level {
							let mut subdir_files = self::get_files_recursively(
								&path,
								&Level::SubDirStatic,
							);
							result.append(&mut subdir_files);
						} else {
							panic!("Didn't expect to find {} except directly under input directory root.", path.display());
						}
					} else if file_name.starts_with('_') {
						println!(
							"Skipping '_'-prefixed dir: {}",
							path.display()
						);
					} else if file_name.starts_with('.') {
						println!(
							"Skipping '.'-prefixed dir: {}",
							path.display()
						);
					} else {
						let mut subdir_files =
							self::get_files_recursively(&path, &Level::SubDir);
						result.append(&mut subdir_files);
					}
				}
			}
		} else {
			println!("Skipping non-file/dir {}", path.display());
		}
	}

	result
}

pub fn process_file(
	input_file_path: &PathBuf,
	output_file_path: &PathBuf,
	front_matter: &Arc<FrontMatter>,
	root_input_dir: &PathBuf,
	root_output_dir: &PathBuf,
	input_output_map: &HashMap<PathBuf, GroupedOptionOutputFile>,
	groups: &HashMap<String, Vec<InputFile>>,
	site_info: &SiteInfo,
) -> GeneratedFile {
	assert_eq!(
		input_file_path.extension(),
		Some(OsStr::new(util::MARKDOWN_EXTENSION))
	);

	let timer = Instant::now();

	let mut input_file =
		BufReader::new(fs::File::open(&input_file_path).unwrap_or_else(|e| {
			panic!("Failed opening \"{}\": {}.", &input_file_path.display(), e)
		}));

	input_file
		.seek(SeekFrom::Start(front_matter.end_position))
		.unwrap_or_else(|e| {
			panic!("Failed seeking in {}: {}", input_file_path.display(), e)
		});

	let mut processed_markdown_content = BufWriter::new(Vec::new());

	liquid::process(
		&mut input_file,
		&mut processed_markdown_content,
		HashMap::new(),
		&liquid::Context {
			input_file_path,
			output_file_path,
			front_matter,
			html_content: None,
			root_input_dir,
			root_output_dir,
			input_output_map,
			groups,
			site_info,
		},
	);

	let markdown_content = String::from_utf8_lossy(
		&processed_markdown_content
			.into_inner()
			.unwrap_or_else(|e| panic!("into_inner() failed: {}", e)),
	)
	.to_string();

	let mut output_buf = BufWriter::new(Vec::new());
	let template_path_result =
		compute_template_path(input_file_path, root_input_dir);

	let mut html_content = String::with_capacity(markdown_content.len());
	html::push_html(&mut html_content, Parser::new_ext(&markdown_content, Options::ENABLE_TABLES));

	let mut template_file = BufReader::new(
		fs::File::open(&template_path_result.path).unwrap_or_else(|e| {
			panic!(
				"Failed opening template file {}: {}",
				template_path_result.path.display(),
				e
			)
		}),
	);

	liquid::process(
		&mut template_file,
		&mut output_buf,
		HashMap::new(),
		&liquid::Context {
			input_file_path: &template_path_result.path,
			output_file_path,
			front_matter,
			html_content: Some(&html_content),
			root_input_dir,
			root_output_dir,
			input_output_map,
			groups,
			site_info,
		},
	);

	write_buffer_to_file(output_buf.buffer(), output_file_path);

	println!(
		"Converted {} to {} (using template {}) in {} ms.",
		input_file_path.display(),
		&output_file_path.display(),
		template_path_result.path.display(),
		timer.elapsed().as_millis()
	);

	GeneratedFile {
		file: OutputFile {
			front_matter: front_matter.clone(),
			path: strip_prefix(output_file_path, root_output_dir),
		},
		group: template_path_result.group,
		html_content,
	}
}

pub fn reindex(
	input_file_path: &PathBuf,
	grouped_file: &GroupedOutputFile,
	input_dir: &PathBuf,
	output_dir: &PathBuf,
	input_output_map: &mut HashMap<PathBuf, GroupedOptionOutputFile>,
	groups: &mut HashMap<String, Vec<InputFile>>,
	tags: &mut HashMap<String, Vec<InputFile>>,
	site_info: &SiteInfo,
) {
	let previous = input_output_map
		.insert(input_file_path.clone(), grouped_file.clone_to_option());

	if let Some(group) = &grouped_file.group {
		if match &previous {
			Some(previous) => previous.group.as_deref() != Some(group),
			None => true,
		} {
			let input_file_rec = InputFile {
				path: input_file_path.clone(),
				front_matter: grouped_file.file.front_matter.clone(),
			};

			match groups.entry(group.clone()) {
				Entry::Vacant(ve) => {
					ve.insert(vec![input_file_rec]);
				}
				Entry::Occupied(oe) => {
					let v = oe.into_mut();
					if let Some(e) =
						v.iter_mut().find(|f| f.path == input_file_rec.path)
					{
						*e = input_file_rec
					} else {
						v.push(input_file_rec)
					}
				}
			}
		}
	} else if let Some(previous) = &previous {
		if let Some(group) = &previous.group {
			let entries = groups.get_mut(group).unwrap_or_else(|| {
				panic!("Expecting to find {} group.", group)
			});
			entries.retain(|f| &f.path != input_file_path)
		}
	}

	for tag in &grouped_file.file.front_matter.tags {
		if let Some(previous) = &previous {
			if let Some(prev_fm) = &previous.file.front_matter {
				if prev_fm.tags.contains(tag) {
					continue;
				}
			}
		}

		let tags_file = PathBuf::from("tags")
			.join(&tag)
			.with_extension(util::HTML_EXTENSION);
		let file = InputFile {
			front_matter: grouped_file.file.front_matter.clone(),
			path: input_file_path.clone(),
		};
		let entries = match tags.entry(tag.clone()) {
			Entry::Vacant(ve) => {
				let prev = input_output_map.insert(
					input_dir.join(&tags_file),
					GroupedOptionOutputFile {
						file: OptionOutputFile {
							path: output_dir.join(&tags_file),
							front_matter: Some(Arc::new(FrontMatter {
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
				);
				assert!(prev.is_none());
				ve.insert(vec![file])
			}
			Entry::Occupied(oe) => {
				let v = oe.into_mut();
				assert!(v.iter().find(|f| f.path == file.path).is_none());
				v.push(file);
				v
			}
		};
		entries.sort_by(|lhs, rhs| {
			rhs.front_matter.date.cmp(&lhs.front_matter.date)
		});

		generate_tag_file(
			&input_dir.join(tags_file),
			entries,
			input_dir,
			output_dir,
			input_output_map,
			groups,
			site_info,
		);
	}

	// TODO: Remove when tags are removed?
}

pub fn process_template_file(
	input_file_path: &PathBuf,
	output_file_path: &PathBuf,
	front_matter: &Arc<FrontMatter>,
	root_input_dir: &PathBuf,
	root_output_dir: &PathBuf,
	input_output_map: &HashMap<PathBuf, GroupedOptionOutputFile>,
	groups: &HashMap<String, Vec<InputFile>>,
	site_info: &SiteInfo,
) {
	assert_eq!(
		input_file_path.extension(),
		Some(OsStr::new(util::HTML_EXTENSION))
	);

	let timer = Instant::now();

	let mut input_file =
		BufReader::new(fs::File::open(&input_file_path).unwrap_or_else(|e| {
			panic!("Failed opening \"{}\": {}.", &input_file_path.display(), e)
		}));

	input_file
		.seek(SeekFrom::Start(front_matter.end_position))
		.unwrap_or_else(|e| {
			panic!("Failed seeking in {}: {}", input_file_path.display(), e)
		});

	let mut output_buf = BufWriter::new(Vec::new());

	liquid::process(
		&mut input_file,
		&mut output_buf,
		HashMap::new(),
		&liquid::Context {
			input_file_path,
			output_file_path,
			front_matter,
			html_content: None,
			root_input_dir,
			root_output_dir,
			input_output_map,
			groups,
			site_info,
		},
	);

	write_buffer_to_file(output_buf.buffer(), output_file_path);

	println!(
		"Processed markdown-less {} to {} in {} ms.",
		input_file_path.display(),
		output_file_path.display(),
		timer.elapsed().as_millis(),
	);
}

pub fn generate_tag_file(
	input_file_path: &PathBuf,
	entries: &[InputFile],
	root_input_dir: &PathBuf,
	root_output_dir: &PathBuf,
	input_output_map: &HashMap<PathBuf, GroupedOptionOutputFile>,
	groups: &HashMap<String, Vec<InputFile>>,
	site_info: &SiteInfo,
) {
	assert_eq!(
		input_file_path.extension(),
		Some(OsStr::new(util::HTML_EXTENSION))
	);

	let timer = Instant::now();

	let grouped_file =
		input_output_map.get(input_file_path).unwrap_or_else(|| {
			panic!(
				"Failed finding {} in {:?}",
				input_file_path.display(),
				input_output_map.keys()
			)
		});

	let template_file = root_input_dir.join("_layouts/tag.html");
	let mut input_file =
		BufReader::new(fs::File::open(&template_file).unwrap_or_else(|e| {
			panic!("Failed opening \"{}\": {}.", &template_file.display(), e)
		}));

	let output_file_path = &grouped_file.file.path;
	let front_matter =
		grouped_file.file.front_matter.as_ref().unwrap_or_else(|| {
			panic!(
				"Expecting at least a default FrontMatter instance on file: {}",
				output_file_path.display()
			)
		});

	let mut output_buf = BufWriter::new(Vec::new());

	let mut entries_as_values = Vec::new();
	for entry in entries {
		let mut map = HashMap::new();
		let mut link = String::from("/");
		link.push_str(
			&strip_prefix(&entry.path, root_input_dir).to_string_lossy(),
		);
		map.insert("link", liquid::Value::String(link));
		map.insert(
			"title",
			liquid::Value::String(entry.front_matter.title.clone()),
		);
		map.insert(
			"date",
			liquid::Value::String(
				entry
					.front_matter
					.date
					.as_ref()
					.map_or_else(String::new, String::clone),
			),
		);
		entries_as_values.push(liquid::Value::Dictionary { map });
	}
	let mut outer_variables = HashMap::new();
	outer_variables.insert(
		"entries".to_string(),
		liquid::Value::List {
			values: entries_as_values,
		},
	);

	liquid::process(
		&mut input_file,
		&mut output_buf,
		outer_variables,
		&liquid::Context {
			input_file_path: &template_file,
			output_file_path,
			front_matter,
			html_content: None,
			root_input_dir,
			root_output_dir,
			input_output_map,
			groups,
			site_info,
		},
	);

	write_buffer_to_file(output_buf.buffer(), output_file_path);

	println!(
		"Generated tags file {} in {} ms.",
		output_file_path.display(),
		timer.elapsed().as_millis(),
	);
}

fn write_buffer_to_file(buffer: &[u8], path: &PathBuf) {
	let closest_output_dir = path.parent().unwrap_or_else(|| {
		panic!(
			"Output file path without a parent directory?: {}",
			path.display()
		)
	});
	fs::create_dir_all(closest_output_dir).unwrap_or_else(|e| {
		panic!(
			"Failed creating directories for {}: {}",
			closest_output_dir.display(),
			e
		)
	});

	let mut output_file = fs::File::create(&path).unwrap_or_else(|e| {
		panic!("Failed creating \"{}\": {}.", &path.display(), e)
	});
	output_file.write_all(buffer).unwrap_or_else(|e| {
		panic!("Failed writing to \"{}\": {}.", &path.display(), e)
	});

	// Avoiding sync_all() for now to be friendlier to disks.
	output_file.sync_data().unwrap_or_else(|e| {
		panic!("Failed sync_data() for \"{}\": {}.", &path.display(), e)
	});
}

pub fn parse_fm_and_compute_output_path(
	input_file_path: &PathBuf,
	root_input_dir: &PathBuf,
	root_output_dir: &PathBuf,
) -> GroupedOutputFile {
	let mut path = root_output_dir.clone();
	if input_file_path.starts_with(root_input_dir) {
		path.push(
			strip_prefix(input_file_path, root_input_dir)
				.with_extension(util::HTML_EXTENSION),
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
				strip_prefix(input_file_path, &full_root_input_path)
					.with_extension(util::HTML_EXTENSION),
			);
		} else {
			panic!(
				"Unable to handle input file name: {}",
				input_file_path.display()
			)
		}
	}

	let mut input_file =
		BufReader::new(fs::File::open(&input_file_path).unwrap_or_else(|e| {
			panic!("Failed opening \"{}\": {}.", &input_file_path.display(), e)
		}));

	let front_matter =
		Arc::new(crate::front_matter::parse(input_file_path, &mut input_file));

	let mut group = None;
	let input_file_parent = input_file_path
		.parent()
		.unwrap_or_else(|| {
			panic!("Failed to get parent from: {}", input_file_path.display())
		})
		.file_stem()
		.unwrap_or_else(|| {
			panic!(
				"Expected file stem on parent of: {}",
				input_file_path.display()
			)
		})
		.to_string_lossy();
	if input_file_parent.ends_with('s') {
		group = Some(input_file_parent.to_string());
	}

	GroupedOutputFile {
		file: OutputFile { path, front_matter },
		group,
	}
}

fn compute_template_path(
	input_file_path: &PathBuf,
	root_input_dir: &PathBuf,
) -> ComputedTemplatePath {
	let mut template_file_path = root_input_dir.join(PathBuf::from("_layouts"));
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
	let mut group = None;
	if template_name.ends_with('s') {
		group = Some(template_name.clone());
		template_name.truncate(template_name.len() - 1)
	}
	template_file_path.push(template_name);
	template_file_path.set_extension("html");
	if !template_file_path.exists() {
		let mut default_template = template_file_path.clone();
		default_template.set_file_name("default.html");
		if !default_template.exists() {
			panic!(
				"Failed resolving template file for: {}, tried with {} and {}",
				input_file_path.display(),
				template_file_path.display(),
				default_template.display(),
			);
		}
		template_file_path = default_template;
	}

	ComputedTemplatePath {
		path: template_file_path,
		group,
	}
}
