use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use yaml_rust::YamlLoader;

#[derive(Clone)]
pub struct FrontMatter {
	pub title: String,
	pub date: String,
	pub published: bool,
	pub edited: Option<String>,
	pub categories: Vec<String>,
	pub tags: Vec<String>,
	pub layout: Option<String>,
	pub custom_attributes: BTreeMap<String, String>,
	pub end_position: u64,
}

pub fn parse(
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
		end_position: 0,
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
		for mapping in hash {
			if let yaml_rust::Yaml::String(s) = mapping.0 {
				parse_yaml_attribute(&mut result, s, mapping.1, input_file_path)
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
