use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;

use yaml_rust::YamlLoader;

use crate::util;

#[derive(Clone)]
pub struct FrontMatter {
	pub title: String,
	pub date: Option<String>,
	pub published: bool,
	pub edited: Option<String>,
	pub categories: Vec<String>,
	pub tags: Vec<String>,
	pub layout: Option<String>,
	pub custom_attributes: BTreeMap<String, String>,
	pub end_position: u64,
	pub subsequent_line: usize,
}

pub fn parse(
	input_file_path: &PathBuf,
	reader: &mut BufReader<fs::File>,
) -> FrontMatter {
	const MAX_FRONT_MATTER_LINES: u8 = 16;

	let mut result = FrontMatter {
		title: String::new(),
		date: None,
		published: true,
		edited: None,
		categories: Vec::new(),
		tags: Vec::new(),
		layout: None,
		custom_attributes: BTreeMap::new(),
		end_position: 0,
		subsequent_line: 1,
	};

	let mut line = String::new();
	let first_line_len = reader.read_line(&mut line).unwrap_or_else(|e| {
		panic!(
			"Failed reading first line from \"{}\": {}.",
			input_file_path.display(),
			e
		)
	});

	if first_line_len == 4 && line == "---\n" {
		result.subsequent_line = 2;
		println!("Found front matter in: {}", input_file_path.display());
		let mut front_matter_str = String::new();
		let mut line_count = 0;
		loop {
			result.subsequent_line += 1;
			line.clear();
			let _line_len = reader.read_line(&mut line).unwrap_or_else(|e| {
				panic!(
					"Failed reading line from \"{}\": {}.",
					input_file_path.display(),
					e
				)
			});
			if line == "---\n" {
				result.end_position =
					reader.seek(SeekFrom::Current(0)).unwrap_or_else(|e| {
						panic!(
							"Failed getting current buffer position of file {}: {}",
							input_file_path.display(),
							e
						)
					});
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
			for (key, value) in hash {
				if let yaml_rust::Yaml::String(key) = key {
					parse_yaml_attribute(
						&mut result,
						key,
						value,
						input_file_path,
					)
				} else {
					panic!("Expected string keys in YAML element in front matter of \"{}\" but got {:?}.", 
							input_file_path.display(), &key)
				}
			}
		} else {
			panic!("Expected Hash as YAML root element in front matter of \"{}\" but got {:?}.", 
				input_file_path.display(), &yaml[0])
		}
	}

	fixup_date(input_file_path, &mut result);

	if !result.published
		&& input_file_path.extension()
			!= Some(OsStr::new(util::MARKDOWN_EXTENSION))
	{
		panic!("Only support turning off publishing for markdown files, found attempt to do it for: {}", input_file_path.display());
	}

	result
}

fn fixup_date(input_file_path: &PathBuf, front_matter: &mut FrontMatter) {
	if let Some(date) = &front_matter.date {
		if date != "auto" {
			if let Some(edited) = &front_matter.edited {
				if edited != "auto" {
					// If neither date nor edited are set to "auto", bail.
					return;
				}
			} else {
				// If date is not set to "auto", and edited is unset, bail.
				return;
			}
		}
	}

	println!("Published or edited dates not specified or set to \"auto\" in front matter of {}, fetching modified date from file system..", input_file_path.display());

	let metadata = fs::metadata(input_file_path).unwrap_or_else(|e| {
		panic!(
			"Failed fetching metadata for {}: {}",
			input_file_path.display(),
			e
		)
	});

	let modified = metadata.modified().unwrap_or_else(|e| {
		panic!(
			"Failed fetching modified time for {}: {}",
			input_file_path.display(),
			e
		)
	});

	let fs_time = Some(humantime::format_rfc3339_seconds(modified).to_string());

	if front_matter.date.as_deref() == Some("auto") {
		front_matter.date = fs_time;
		if front_matter.edited.is_some() {
			panic!("Can't have date set to \"auto\" while also specifying edited in front matter of {}", input_file_path.display());
		}
	} else if (front_matter.date.is_none() && front_matter.edited.is_none())
		|| front_matter.edited.as_deref() == Some("auto")
	{
		front_matter.edited = fs_time;
	}
}

fn parse_yaml_attribute(
	front_matter: &mut FrontMatter,
	key: &str,
	value: &yaml_rust::Yaml,
	input_file_path: &PathBuf,
) {
	match key {
		"title" => {
			if let yaml_rust::Yaml::String(value) = value {
				front_matter.title = value.clone();
			} else {
				panic!(
					"title of \"{}\" has unexpected type {:?}",
					input_file_path.display(),
					value
				)
			}
		}
		"date" => {
			if let yaml_rust::Yaml::String(value) = value {
				front_matter.date = Some(value.clone());
			} else {
				panic!(
					"date of \"{}\" has unexpected type {:?}",
					input_file_path.display(),
					value
				)
			}
		}
		"published" => {
			if let yaml_rust::Yaml::Boolean(value) = value {
				front_matter.published = *value;
			} else {
				panic!(
					"published of \"{}\" has unexpected type {:?}",
					input_file_path.display(),
					value
				)
			}
		}
		"edited" => {
			if let yaml_rust::Yaml::String(value) = value {
				front_matter.edited = Some(value.clone());
			} else {
				panic!(
					"edited of \"{}\" has unexpected type {:?}",
					input_file_path.display(),
					value
				)
			}
		}
		"categories" => {
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
		}
		"tags" => {
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
		}
		"layout" => {
			if let yaml_rust::Yaml::String(value) = value {
				front_matter.layout = Some(value.clone());
			} else {
				panic!(
					"layout of \"{}\" has unexpected type {:?}",
					input_file_path.display(),
					value
				)
			}
		}
		_ => {
			if let yaml_rust::Yaml::String(value) = value {
				front_matter
					.custom_attributes
					.insert(key.to_string(), value.clone());
			} else {
				panic!(
					"custom attribute \"{}\" of \"{}\" has unexpected type {:?}",
					key,
					input_file_path.display(),
					value
				)
			}
		}
	}
}
