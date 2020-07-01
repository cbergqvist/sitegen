use std::collections::BTreeMap;
use std::collections::HashMap;
use std::io::{BufReader, BufWriter, Cursor};
use std::path::PathBuf;

use crate::front_matter;
use crate::liquid;
use crate::markdown;

#[test]
fn test_liquid_link() {
	let input_file_path = PathBuf::from("./input/virtual_test.md");
	let output_file_path = PathBuf::from("./output/virtual_test.html");
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
	let mut input_file = BufReader::new(Cursor::new(
		(r#"[Foo]({% link /virtual_test.md %})"#).as_bytes(),
	));

	let mut input_output_map = HashMap::new();
	input_output_map.insert(
		input_file_path.clone(),
		markdown::OptionOutputFile {
			front_matter: Some(front_matter.clone()),
			path: output_file_path.clone(),
		},
	);
	let groups = HashMap::new();

	let mut processed_markdown_content = BufWriter::new(Vec::new());
	liquid::process(
		&mut input_file,
		&mut processed_markdown_content,
		&input_output_map,
		&groups,
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
	let mut input_file = BufReader::new(Cursor::new((r#"{% "#).as_bytes()));

	let mut input_output_map = HashMap::new();
	input_output_map.insert(
		input_file_path.clone(),
		markdown::OptionOutputFile {
			front_matter: Some(front_matter.clone()),
			path: output_file_path.clone(),
		},
	);
	let groups = HashMap::new();

	let mut processed_markdown_content = BufWriter::new(Vec::new());
	let result = std::panic::catch_unwind(move || {
		liquid::process(
			&mut input_file,
			&mut processed_markdown_content,
			&input_output_map,
			&groups,
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

#[test]
fn test_liquid_for() {
	let input_file_path_a = PathBuf::from("./input/posts/virtual_test_a.md");
	let input_file_path_b = PathBuf::from("./input/posts/virtual_test_b.md");
	let output_file_path_a =
		PathBuf::from("./output/posts/virtual_test_a.html");
	let output_file_path_b =
		PathBuf::from("./output/posts/virtual_test_b.html");
	let front_matter_a = front_matter::FrontMatter {
		title: "Title A".to_string(),
		date: None,
		published: true,
		edited: None,
		categories: Vec::new(),
		tags: Vec::new(),
		layout: None,
		custom_attributes: BTreeMap::new(),
		end_position: 0,
	};
	let front_matter_b = front_matter::FrontMatter {
		title: "Title B".to_string(),
		date: None,
		published: true,
		edited: None,
		categories: Vec::new(),
		tags: Vec::new(),
		layout: None,
		custom_attributes: BTreeMap::new(),
		end_position: 0,
	};
	let mut input_file = BufReader::new(Cursor::new(
		(r#"{% for post in posts %}-{{ post.title }}-{% endfor %}"#).as_bytes(),
	));

	let mut input_output_map = HashMap::new();
	input_output_map.insert(
		input_file_path_a.clone(),
		markdown::OptionOutputFile {
			front_matter: Some(front_matter_a.clone()),
			path: output_file_path_a.clone(),
		},
	);
	input_output_map.insert(
		input_file_path_b.clone(),
		markdown::OptionOutputFile {
			front_matter: Some(front_matter_b.clone()),
			path: output_file_path_b.clone(),
		},
	);

	let mut groups = HashMap::new();
	groups.insert(
		"posts".to_string(),
		vec![
			markdown::OutputFile {
				front_matter: front_matter_a.clone(),
				path: output_file_path_a.clone(),
			},
			markdown::OutputFile {
				front_matter: front_matter_b.clone(),
				path: output_file_path_b.clone(),
			},
		],
	);

	let mut processed_markdown_content = BufWriter::new(Vec::new());
	liquid::process(
		&mut input_file,
		&mut processed_markdown_content,
		&input_output_map,
		&groups,
		&liquid::Context {
			input_file_path: &input_file_path_a,
			output_file_path: &output_file_path_a,
			front_matter: &front_matter_a,
			html_content: None,
			root_input_dir: &PathBuf::from("./input"),
			root_output_dir: &PathBuf::from("./output"),
		},
	);

	assert_eq!(
		String::from_utf8_lossy(
			&processed_markdown_content.into_inner().unwrap()
		),
		"-Title A--Title B-"
	);
}
