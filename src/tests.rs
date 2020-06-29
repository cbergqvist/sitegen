use std::collections::BTreeMap;
use std::collections::HashMap;
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;

use crate::front_matter;
use crate::liquid;
use crate::markdown;

#[test]
fn test_liquid_link() {
	let input_file_path = PathBuf::from("./input/virtual_test.md");
	let output_file_path = PathBuf::from("./output/virtual_test.html");
	let mut processed_markdown_content = BufWriter::new(Vec::new());
	let front_matter = front_matter::FrontMatter {
		title: "Title".to_string(),
		date: None,
		published: true,
		edited: None,
		categories: Vec::new(),
		tags: Vec::new(),
		layout: None,
		custom_attributes: BTreeMap::new(),
		end_position: 0,
	};
	let mut input_file =
		BufReader::new((r#"[Foo]({% link /virtual_test.md %})"#).as_bytes());

	let mut input_output_map = HashMap::new();
	input_output_map.insert(
		input_file_path.clone(),
		markdown::OutputFile {
			front_matter: Some(front_matter.clone()),
			group: None,
			path: output_file_path.clone(),
		},
	);

	liquid::process(
		&mut input_file,
		&mut processed_markdown_content,
		&input_output_map,
		&liquid::Context {
			input_file_path: &input_file_path,
			output_file_path: &output_file_path,
			front_matter: &front_matter,
			html_content: None,
			root_input_dir: &PathBuf::from("./input"),
			root_output_dir: &PathBuf::from("./output"),
		},
	);

	assert_eq!(
		String::from_utf8_lossy(
			&processed_markdown_content.into_inner().unwrap()
		),
		"[Foo](./virtual_test.html)"
	);
}

#[test]
fn test_liquid_unfinished() {
	let input_file_path = PathBuf::from("./input/virtual_test.md");
	let output_file_path = PathBuf::from("./output/virtual_test.html");
	let mut processed_markdown_content = BufWriter::new(Vec::new());
	let front_matter = front_matter::FrontMatter {
		title: "Title".to_string(),
		date: None,
		published: true,
		edited: None,
		categories: Vec::new(),
		tags: Vec::new(),
		layout: None,
		custom_attributes: BTreeMap::new(),
		end_position: 0,
	};
	let mut input_file = BufReader::new((r#"{% "#).as_bytes());

	let mut input_output_map = HashMap::new();
	input_output_map.insert(
		input_file_path.clone(),
		markdown::OutputFile {
			front_matter: Some(front_matter.clone()),
			group: None,
			path: output_file_path.clone(),
		},
	);

	let result = std::panic::catch_unwind(move || {
		liquid::process(
			&mut input_file,
			&mut processed_markdown_content,
			&input_output_map,
			&liquid::Context {
				input_file_path: &input_file_path,
				output_file_path: &output_file_path,
				front_matter: &front_matter,
				html_content: None,
				root_input_dir: &PathBuf::from("./input"),
				root_output_dir: &PathBuf::from("./output"),
			},
		)
	});

	assert!(result.is_err());
}
