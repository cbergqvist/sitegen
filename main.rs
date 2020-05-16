use std::collections::BTreeMap;
use std::io::BufRead;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;
use std::option::Option;
use std::string::String;
use std::time::Instant;
use std::{fs, io};

use pulldown_cmark::{html, Parser};

const INPUT_PATH: &str = "./input";
const OUTPUT_PATH: &str = "./output";

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

fn main() -> io::Result<()> {
	let markdown_files = get_markdown_files();

	if markdown_files.is_empty() {
		println!("Found no valid file entries under \"{}\".", INPUT_PATH);
		return Ok(());
	}

	fs::create_dir(OUTPUT_PATH).unwrap_or_else(|e| {
		panic!("Failed creating \"{}\": {}.", OUTPUT_PATH, e)
	});

	for file_name in markdown_files {
		process_markdown_file(&file_name)
	}

	Ok(())
}

fn get_markdown_files() -> Vec<std::path::PathBuf> {
	let entries = fs::read_dir(INPUT_PATH).unwrap_or_else(|e| {
		panic!("Failed reading paths from \"{}\": {}.", INPUT_PATH, e)
	});
	let markdown_extension = std::ffi::OsStr::new("md");
	let mut files = Vec::new();
	for entry in entries {
		match entry {
			Ok(entry) => {
				let path = entry.path();
				let ext = path.extension();
				if ext == Some(markdown_extension) {
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
				println!("WARNING: Invalid entry in \"{}\": {}", INPUT_PATH, e)
			}
		}
	}
	return files;
}

fn process_markdown_file(input_file_name: &std::path::PathBuf) {
	fn write_to_output(
		output_buf: &mut io::BufWriter<&mut Vec<u8>>,
		data: &[u8],
	) {
		output_buf.write(data).unwrap_or_else(|e| {
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

	let front_matter = parse_front_matter(&input_file_name_str, &mut reader);
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
<head><title>",
	);
	write_to_output(&mut output_buf, front_matter.title.as_bytes());
	write_to_output(
		&mut output_buf,
		b"</title></head>
<body>",
	);
	html::write_html(&mut output_buf, parser).unwrap_or_else(|e| {
		panic!(
			"Failed converting Markdown file \"{}\" to HTML: {}.",
			&input_file_name.display(),
			e
		)
	});
	write_to_output(
		&mut output_buf,
		b"</body>
</html>",
	);

	let mut output_file_name = String::from(OUTPUT_PATH);
	output_file_name.push_str(
		&input_file_name_str
			[INPUT_PATH.len()..(input_file_name_str.len() - "md".len())],
	);
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
}

fn parse_front_matter(
	input_file_name_str: &str,
	reader: &mut io::BufReader<std::fs::File>,
) -> FrontMatter {
	let mut result = FrontMatter {
		title: input_file_name_str
			[INPUT_PATH.len() + 1..input_file_name_str.len() - 3]
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
			&input_file_name_str, e
		)
	});

	// YAML Front matter present missing?
	if first_line_len != 4 || line != "---\n" {
		reader.seek(SeekFrom::Start(0)).unwrap_or_else(|e| {
			panic!("Failed seeking in \"{}\": {}.", &input_file_name_str, e)
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
			panic!(
				"Failed reading line from \"{}\": {}.",
				&input_file_name_str, e
			)
		});
		if line == "---\n" {
			break;
		} else {
			line_count += 1;
			if line_count > MAX_FRONT_MATTER_LINES {
				panic!("Entered front matter parsing mode but failed to find end after {} lines while parsing {}.", MAX_FRONT_MATTER_LINES, &input_file_name_str);
			}
			front_matter_str.push_str(&line);
		}
	}
	let yaml =
		YamlLoader::load_from_str(&front_matter_str).unwrap_or_else(|e| {
			panic!(
				"Failed loading YAML front matter from \"{}\": {}.",
				&input_file_name_str, e
			)
		});
	if yaml.len() != 1 {
		panic!("Expected only one YAML root element (Hash) in front matter of \"{}\" but got {}.", 
			&input_file_name_str, yaml.len());
	}
	if let yaml_rust::Yaml::Hash(hash) = &yaml[0] {
		for mapping in hash {
			if let yaml_rust::Yaml::String(s) = mapping.0 {
				if s == "title" {
					if let yaml_rust::Yaml::String(value) = mapping.1 {
						result.title = value.clone();
					} else {
						panic!(
							"title of \"{}\" has unexpected type {:?}",
							&input_file_name_str, mapping.1
						)
					}
				} else if s == "date" {
					if let yaml_rust::Yaml::String(value) = mapping.1 {
						result.date = value.clone();
					} else {
						panic!(
							"date of \"{}\" has unexpected type {:?}",
							&input_file_name_str, mapping.1
						)
					}
				} else if s == "published" {
					if let yaml_rust::Yaml::Boolean(value) = mapping.1 {
						result.published = *value;
					} else {
						panic!(
							"published of \"{}\" has unexpected type {:?}",
							&input_file_name_str, mapping.1
						)
					}
				} else if s == "edited" {
					if let yaml_rust::Yaml::String(value) = mapping.1 {
						result.edited = Some(value.clone());
					} else {
						panic!(
							"edited of \"{}\" has unexpected type {:?}",
							&input_file_name_str, mapping.1
						)
					}
				} else if s == "categories" {
					if let yaml_rust::Yaml::Array(value) = mapping.1 {
						for element in value {
							if let yaml_rust::Yaml::String(value) = element {
								result.categories.push(value.clone())
							} else {
								panic!("Element of categories of \"{}\" has unexpected type {:?}",
							&input_file_name_str, element)
							}
						}
					} else {
						panic!(
							"categories of \"{}\" has unexpected type {:?}",
							&input_file_name_str, mapping.1
						)
					}
				} else if s == "tags" {
					if let yaml_rust::Yaml::Array(value) = mapping.1 {
						result.tags.clear();
						for element in value {
							if let yaml_rust::Yaml::String(value) = element {
								result.tags.push(value.clone())
							} else {
								panic!("Element of tags of \"{}\" has unexpected type {:?}",
							&input_file_name_str, element)
							}
						}
					} else {
						panic!(
							"tags of \"{}\" has unexpected type {:?}",
							&input_file_name_str, mapping.1
						)
					}
				} else if s == "layout" {
					if let yaml_rust::Yaml::String(value) = mapping.1 {
						result.layout = Some(value.clone());
					} else {
						panic!(
							"layout of \"{}\" has unexpected type {:?}",
							&input_file_name_str, mapping.1
						)
					}
				} else {
					if let yaml_rust::Yaml::String(value) = mapping.1 {
						result
							.custom_attributes
							.insert(s.to_string(), value.clone());
					} else {
						panic!("custom attribute \"{}\" of \"{}\" has unexpected type {:?}", s,
							&input_file_name_str, mapping.1)
					}

					println!(
						"Skipping unrecognized \"{}\" in \"{}\".",
						s, &input_file_name_str
					)
				}
			} else {
				panic!("Expected string keys in YAML element in front matter of \"{}\" but got {:?}.", 
						&input_file_name_str, &mapping.0)
			}
		}
	} else {
		panic!("Expected Hash as YAML root element in front matter of \"{}\" but got {:?}.", 
			&input_file_name_str, &yaml[0])
	}

	return result;
}
