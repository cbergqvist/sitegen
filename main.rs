use std::io::BufRead;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;
use std::{fs, io};

use pulldown_cmark::{html, Parser};

const INPUT_PATH: &str = "./input";
const OUTPUT_PATH: &str = "./output";

fn main() -> io::Result<()> {
	let entries = fs::read_dir(INPUT_PATH).unwrap_or_else(|e| {
		panic!("Failed reading paths from \"{}\": {}.", INPUT_PATH, e)
	});

	let markdown_extension = std::ffi::OsStr::new("md");
	let mut markdown_files = Vec::new();
	for entry in entries {
		match entry {
			Ok(entry) => {
				let path = entry.path();
				let ext = path.extension();
				if ext == Some(markdown_extension) {
					if let Ok(ft) = entry.file_type() {
						if ft.is_file() {
							markdown_files.push(path);
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

	if markdown_files.is_empty() {
		println!("Found no valid file entries under \"{}\".", INPUT_PATH);
		return Ok(());
	}

	fs::create_dir(OUTPUT_PATH).unwrap_or_else(|e| {
		panic!("Failed creating \"{}\": {}.", OUTPUT_PATH, e)
	});

	fn write_to_output(
		output_buf: &mut io::BufWriter<&mut Vec<u8>>,
		data: &[u8],
	) {
		output_buf.write(data).unwrap_or_else(|e| {
			panic!("Failed writing \"<html>\" to to buffer: {}.", e)
		});
	}

	for input_file_name in markdown_files {
		let input_file = fs::File::open(&input_file_name).unwrap_or_else(|e| {
			panic!("Failed opening \"{}\": {}.", &input_file_name.display(), e)
		});

		let input_file_name_str =
			input_file_name.to_str().unwrap_or_else(|| {
				panic!(
					"Failed converting \"{}\" to str.",
					&input_file_name.display()
				)
			});
		let mut title = input_file_name_str
			[INPUT_PATH.len() + 1..input_file_name_str.len() - 3]
			.to_owned();

		let mut reader = io::BufReader::new(input_file);
		let mut line = String::new();
		let first_line_len = reader.read_line(&mut line).unwrap_or_else(|e| {
			panic!(
				"Failed reading first line from \"{}\": {}.",
				&input_file_name.display(),
				e
			)
		});

		// YAML Front matter present?
		if first_line_len == 4 && line == "---\n" {
			use yaml_rust::YamlLoader;

			let mut front_matter = String::new();
			const MAX_FRONT_MATTER_LINES: u8 = 16;
			let mut line_count = 0;
			loop {
				line.clear();
				let _line_len =
					reader.read_line(&mut line).unwrap_or_else(|e| {
						panic!(
							"Failed reading line from \"{}\": {}.",
							&input_file_name.display(),
							e
						)
					});
				if line == "---\n" {
					break;
				} else {
					line_count += 1;
					if line_count > MAX_FRONT_MATTER_LINES {
						panic!("Entered front matter parsing mode but failed to find end after {} lines while parsing {}.", MAX_FRONT_MATTER_LINES, &input_file_name.display());
					}
					front_matter.push_str(&line);
				}
			}
			let yaml =
				YamlLoader::load_from_str(&front_matter).unwrap_or_else(|e| {
					panic!(
						"Failed loading YAML front matter from \"{}\": {}.",
						&input_file_name.display(),
						e
					)
				});
			if yaml.len() != 1 {
				panic!("Expected only one YAML root element (Hash) in front matter of \"{}\" but got {}.", 
					&input_file_name.display(), yaml.len());
			}
			match &yaml[0] {
				yaml_rust::Yaml::Hash(hash) => {
					for mapping in hash {
						match mapping.0 {
							yaml_rust::Yaml::String(s) => {
								if s == "title" {
									if let yaml_rust::Yaml::String(value) = mapping.1 {
										title = value.clone();
									} else {
										panic!("title of \"{}\" has unexpected type {:?}",
											&input_file_name.display(), mapping.1)
									}
								} else {
									println!("Skipping unrecognized \"{}\" in \"{}\".", s, &input_file_name.display())
								}
							},
							_ => panic!("Expected string keys in YAML element in front matter of \"{}\" but got {:?}.", 
									&input_file_name.display(), &mapping.0)
						}
					}
				},
				_ => panic!("Expected Hash as YAML root element in front matter of \"{}\" but got {:?}.", 
					&input_file_name.display(), &yaml[0])
			}
		} else {
			reader.seek(SeekFrom::Start(0)).unwrap_or_else(|e| {
				panic!(
					"Failed seeking in \"{}\": {}.",
					&input_file_name.display(),
					e
				)
			});
		}

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
		write_to_output(&mut output_buf, title.as_bytes());
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

		let mut output_file = fs::File::create(&output_file_name)
			.unwrap_or_else(|e| {
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
	}

	Ok(())
}
