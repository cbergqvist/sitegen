use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io::{BufReader, BufWriter, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::Instant;

use pulldown_cmark::{html, Parser};

use crate::front_matter::FrontMatter;
use crate::liquid;
use crate::util;

#[derive(Clone)]
pub struct GroupedOutputFile {
	pub file: OptionOutputFile,
	pub group: Option<String>,
}

pub struct ComputedTemplatePath {
	pub path: PathBuf,
	pub group: Option<String>,
}

pub struct GeneratedFile {
	pub file: OutputFile,
	pub group: Option<String>,
	pub html_content: String,
}

#[derive(Clone)]
pub struct OutputFile {
	pub front_matter: FrontMatter,
	pub path: PathBuf,
}

#[derive(Clone)]
pub struct OptionOutputFile {
	pub front_matter: Option<FrontMatter>,
	pub path: PathBuf,
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
		self.html.is_empty() || self.markdown.is_empty() || self.raw.is_empty()
	}

	fn append(&mut self, other: &mut Self) {
		self.html.append(&mut other.html);
		self.markdown.append(&mut other.markdown);
		self.raw.append(&mut other.raw);
	}
}

pub fn get_files(input_dir: &PathBuf) -> InputFileCollection {
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
		match entry {
			Ok(entry) => {
				let path = entry.path();
				if let Ok(ft) = entry.file_type() {
					if ft.is_file() {
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
						} else if file_name.to_string_lossy().starts_with('.') {
							println!(
								"Skipping '.'-prefixed dir: {}",
								path.display()
							);
						} else {
							let mut subdir_files = self::get_files(&path);
							result.append(&mut subdir_files);
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

	result
}

pub fn process_file(
	input_file_path: &PathBuf,
	root_input_dir: &PathBuf,
	root_output_dir: &PathBuf,
	input_output_map: &mut HashMap<PathBuf, OptionOutputFile>,
	groups: &mut HashMap<String, Vec<OutputFile>>,
) -> GeneratedFile {
	assert_eq!(
		input_file_path.extension(),
		Some(OsStr::new(util::MARKDOWN_EXTENSION))
	);

	let timer = Instant::now();

	let output_file = input_output_map
		.entry(input_file_path.clone())
		.or_insert_with(|| {
			let grouped_file = compute_output_path(
				input_file_path,
				root_input_dir,
				root_output_dir,
			);
			if let Some(group) = grouped_file.group {
				let file = OutputFile {
					front_matter: grouped_file
						.file
						.front_matter
						.clone()
						.expect(&format!("Expect front matter for markdown files, but didn't get one for {}.", input_file_path.display())),
					path: grouped_file.file.path.clone(),
				};
				match groups.entry(group) {
					Entry::Vacant(ve) => {
						ve.insert(vec![file]);
					}
					Entry::Occupied(oe) => oe.into_mut().push(file),
				}
			}
			grouped_file.file
		})
		.clone();

	let mut input_file =
		BufReader::new(fs::File::open(&input_file_path).unwrap_or_else(|e| {
			panic!("Failed opening \"{}\": {}.", &input_file_path.display(), e)
		}));

	let front_matter = if let Some(fm) = output_file.front_matter {
		fm
	} else {
		panic!(
			"Expecting at least a default FrontMatter instance on file: {}",
			output_file.path.display()
		)
	};
	input_file
		.seek(SeekFrom::Start(front_matter.end_position))
		.unwrap_or_else(|e| {
			panic!("Failed seeking in {}: {}", input_file_path.display(), e)
		});

	let output_file_path = output_file.path;

	let mut processed_markdown_content = BufWriter::new(Vec::new());

	liquid::process(
		&mut input_file,
		&mut processed_markdown_content,
		input_output_map,
		groups,
		&liquid::Context {
			input_file_path,
			output_file_path: &output_file_path,
			front_matter: &front_matter,
			html_content: None,
			root_input_dir,
			root_output_dir,
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

	let mut html_content = String::new();
	html::push_html(&mut html_content, Parser::new(&markdown_content));

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
		input_output_map,
		groups,
		&liquid::Context {
			input_file_path: &template_path_result.path,
			output_file_path: &output_file_path,
			front_matter: &front_matter,
			html_content: Some(&html_content),
			root_input_dir,
			root_output_dir,
		},
	);

	write_buffer_to_file(output_buf.buffer(), &output_file_path);

	println!(
		"Converted {} to {} (using template {}) in {} ms.",
		input_file_path.display(),
		&output_file_path.display(),
		template_path_result.path.display(),
		timer.elapsed().as_millis()
	);

	GeneratedFile {
		file: OutputFile {
			front_matter: front_matter,
			path: output_file_path
				.strip_prefix(root_output_dir)
				.unwrap_or_else(|e| {
					panic!(
						"Failed stripping prefix \"{}\" from \"{}\": {}",
						root_output_dir.display(),
						output_file_path.display(),
						e
					)
				})
				.to_path_buf(),
		},
		group: template_path_result.group,
		html_content,
	}
}

pub fn process_template_file(
	input_file_path: &PathBuf,
	root_input_dir: &PathBuf,
	root_output_dir: &PathBuf,
	input_output_map: &mut HashMap<PathBuf, OptionOutputFile>,
	groups: &mut HashMap<String, Vec<OutputFile>>,
) -> PathBuf {
	assert_eq!(
		input_file_path.extension(),
		Some(OsStr::new(util::HTML_EXTENSION))
	);

	let timer = Instant::now();

	let output_file = input_output_map
		.entry(input_file_path.clone())
		.or_insert_with(|| {
			compute_output_path(
				input_file_path,
				root_input_dir,
				root_output_dir,
			)
			.file
		})
		.clone();

	let mut input_file =
		BufReader::new(fs::File::open(&input_file_path).unwrap_or_else(|e| {
			panic!("Failed opening \"{}\": {}.", &input_file_path.display(), e)
		}));

	let front_matter = if let Some(fm) = output_file.front_matter {
		fm
	} else {
		panic!(
			"Expecting at least a default FrontMatter instance on file: {}",
			output_file.path.display()
		)
	};
	input_file
		.seek(SeekFrom::Start(front_matter.end_position))
		.unwrap_or_else(|e| {
			panic!("Failed seeking in {}: {}", input_file_path.display(), e)
		});

	let output_file_path = output_file.path;

	let mut output_buf = BufWriter::new(Vec::new());

	liquid::process(
		&mut input_file,
		&mut output_buf,
		input_output_map,
		groups,
		&liquid::Context {
			input_file_path,
			output_file_path: &output_file_path,
			front_matter: &front_matter,
			html_content: None,
			root_input_dir,
			root_output_dir,
		},
	);

	write_buffer_to_file(output_buf.buffer(), &output_file_path);

	println!(
		"Processed markdown-less {} to {} in {} ms.",
		input_file_path.display(),
		output_file_path.display(),
		timer.elapsed().as_millis(),
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

pub fn compute_output_path(
	input_file_path: &PathBuf,
	root_input_dir: &PathBuf,
	root_output_dir: &PathBuf,
) -> GroupedOutputFile {
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

	let mut input_file =
		BufReader::new(fs::File::open(&input_file_path).unwrap_or_else(|e| {
			panic!("Failed opening \"{}\": {}.", &input_file_path.display(), e)
		}));

	let front_matter =
		crate::front_matter::parse(input_file_path, &mut input_file);

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
		file: OptionOutputFile {
			path,
			front_matter: Some(front_matter),
		},
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
