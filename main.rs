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

	for input_file_name in markdown_files {
		let input_file =
			fs::read_to_string(&input_file_name).unwrap_or_else(|e| {
				panic!(
					"Failed opening \"{}\": {}.",
					&input_file_name.display(),
					e
				)
			});

		let parser = Parser::new(input_file.as_str());
		let mut output = Vec::new();
		let mut output_buf = io::BufWriter::new(&mut output);
		html::write_html(&mut output_buf, parser).unwrap_or_else(|e| {
			panic!(
				"Failed converting Markdown file \"{}\" to HTML: {}.",
				&input_file_name.display(),
				e
			)
		});

		let mut output_file_name = String::from(OUTPUT_PATH);
		let input_file_name_str =
			input_file_name.to_str().unwrap_or_else(|| {
				panic!(
					"Failed converting \"{}\" to str.",
					&input_file_name.display()
				)
			});
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
