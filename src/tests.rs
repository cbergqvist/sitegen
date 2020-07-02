use std::collections::{BTreeMap, HashMap};
use std::io::{BufReader, BufWriter, Cursor};
use std::path::PathBuf;

use crate::front_matter::FrontMatter;
use crate::liquid;
use crate::markdown;

fn create_front_matter(title: &str, date: Option<&str>) -> FrontMatter {
	FrontMatter {
		title: title.to_string(),
		date: date.map(|s| s.to_string()),
		published: true,
		edited: None,
		categories: Vec::new(),
		tags: Vec::new(),
		layout: None,
		custom_attributes: BTreeMap::new(),
		end_position: 0,
	}
}

#[test]
fn test_liquid_link() {
	let input_file_path = PathBuf::from("./input/virtual_test.md");
	let output_file_path = PathBuf::from("./output/virtual_test.html");
	let front_matter = create_front_matter("Title", None);
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
		&liquid::Context {
			input_file_path: &input_file_path,
			output_file_path: &output_file_path,
			front_matter: &front_matter,
			html_content: None,
			root_input_dir: &PathBuf::from("./input"),
			root_output_dir: &PathBuf::from("./output"),
			input_output_map: &input_output_map,
			groups: &groups,
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
#[should_panic(
	expected = "Content of ./input/virtual_test.md ended while still in state: TagStart"
)]
fn test_liquid_unfinished() {
	let input_file_path = PathBuf::from("./input/virtual_test.md");
	let output_file_path = PathBuf::from("./output/virtual_test.html");
	let front_matter = create_front_matter("Title", None);
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

	// Expecting panic here:
	liquid::process(
		&mut input_file,
		&mut processed_markdown_content,
		&liquid::Context {
			input_file_path: &input_file_path,
			output_file_path: &output_file_path,
			front_matter: &front_matter,
			html_content: None,
			root_input_dir: &PathBuf::from("./input"),
			root_output_dir: &PathBuf::from("./output"),
			input_output_map: &input_output_map,
			groups: &groups,
		},
	);
}

#[test]
fn test_liquid_for() {
	let input_file_path_a = PathBuf::from("./input/posts/virtual_test_a.md");
	let input_file_path_b = PathBuf::from("./input/posts/virtual_test_b.md");
	let output_file_path_a =
		PathBuf::from("./output/posts/virtual_test_a.html");
	let output_file_path_b =
		PathBuf::from("./output/posts/virtual_test_b.html");
	let front_matter_a =
		create_front_matter("Title A", Some("2001-01-19T20:10:01Z"));
	let front_matter_b = create_front_matter("Title B", None);

	let mut input_file = BufReader::new(Cursor::new(
		(r#"{% for post in posts %}-{{ post.date }} <a href="{{ post.link }}">{{ post.title }}</a>-{% endfor %}"#).as_bytes(),
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
		&liquid::Context {
			input_file_path: &input_file_path_a,
			output_file_path: &output_file_path_a,
			front_matter: &front_matter_a,
			html_content: None,
			root_input_dir: &PathBuf::from("./input"),
			root_output_dir: &PathBuf::from("./output"),
			input_output_map: &input_output_map,
			groups: &groups,
		},
	);

	assert_eq!(
		String::from_utf8_lossy(
			&processed_markdown_content.into_inner().unwrap()
		),
		"-2001-01-19T20:10:01Z <a href=\"./virtual_test_a.html\">Title A</a>-- <a href=\"./virtual_test_b.html\">Title B</a>-"
	);
}

#[test]
fn test_liquid_upcase() {
	let input_file_path = PathBuf::from("./input/virtual_test.md");
	let output_file_path = PathBuf::from("./output/virtual_test.html");
	let front_matter = create_front_matter("Title", None);
	let mut input_file = BufReader::new(Cursor::new(
		(r#"{{ page.title | upcase }}"#).as_bytes(),
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
		&liquid::Context {
			input_file_path: &input_file_path,
			output_file_path: &output_file_path,
			front_matter: &front_matter,
			html_content: None,
			root_input_dir: &PathBuf::from("./input"),
			root_output_dir: &PathBuf::from("./output"),
			input_output_map: &input_output_map,
			groups: &groups,
		},
	);

	assert_eq!(
		String::from_utf8_lossy(
			&processed_markdown_content.into_inner().unwrap()
		),
		"TITLE"
	);
}

#[test]
fn test_liquid_upcase_downcase() {
	let input_file_path = PathBuf::from("./input/virtual_test.md");
	let output_file_path = PathBuf::from("./output/virtual_test.html");
	let front_matter = create_front_matter("Title", None);
	let mut input_file = BufReader::new(Cursor::new(
		(r#"{{ page.title | upcase | downcase }}"#).as_bytes(),
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
		&liquid::Context {
			input_file_path: &input_file_path,
			output_file_path: &output_file_path,
			front_matter: &front_matter,
			html_content: None,
			root_input_dir: &PathBuf::from("./input"),
			root_output_dir: &PathBuf::from("./output"),
			input_output_map: &input_output_map,
			groups: &groups,
		},
	);

	assert_eq!(
		String::from_utf8_lossy(
			&processed_markdown_content.into_inner().unwrap()
		),
		"title"
	);
}
