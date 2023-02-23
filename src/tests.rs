use std::collections::{BTreeMap, HashMap};
use std::io::{BufReader, BufWriter, Cursor};
use std::path::PathBuf;
use std::sync::Arc;

use crate::front_matter::FrontMatter;
use crate::liquid;
use crate::markdown::{GroupedOptionOutputFile, InputFile, OptionOutputFile};
use crate::util::SiteInfo;

fn make_front_matter(title: &str, date: Option<&str>) -> Arc<FrontMatter> {
	Arc::new(FrontMatter {
		title: title.to_string(),
		date: date.map(|s| s.to_string()),
		published: true,
		edited: None,
		categories: Vec::new(),
		tags: Vec::new(),
		layout: None,
		custom_attributes: BTreeMap::new(),
		end_position: 0,
		subsequent_line: 1,
	})
}

#[test]
fn test_liquid_link() {
	let input_file_path = PathBuf::from("./input/virtual_test.md");
	let output_file_path = PathBuf::from("./output/virtual_test.html");
	let front_matter = make_front_matter("Title", None);
	let mut input_file = BufReader::new(Cursor::new(
		(r#"[Foo]({% link "/virtual_test.md" %})"#).as_bytes(),
	));

	let mut input_output_map = HashMap::new();
	input_output_map.insert(
		input_file_path.clone(),
		GroupedOptionOutputFile {
			file: OptionOutputFile {
				front_matter: Some(front_matter.clone()),
				path: output_file_path.clone(),
			},
			group: None,
		},
	);

	let mut processed_markdown_content = BufWriter::new(Vec::new());
	liquid::process(
		&mut input_file,
		&mut processed_markdown_content,
		HashMap::new(),
		&liquid::Context {
			input_file_path: &input_file_path,
			output_file_path: &output_file_path,
			front_matter: &front_matter,
			html_content: None,
			root_input_dir: &PathBuf::from("./input"),
			root_output_dir: &PathBuf::from("./output"),
			input_output_map: &input_output_map,
			groups: &HashMap::new(),
			site_info: &SiteInfo { title: "Site" },
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
	let front_matter = make_front_matter("Title", None);
	let mut input_file = BufReader::new(Cursor::new((r#"{% "#).as_bytes()));

	let mut input_output_map = HashMap::new();
	input_output_map.insert(
		input_file_path.clone(),
		GroupedOptionOutputFile {
			file: OptionOutputFile {
				front_matter: Some(front_matter.clone()),
				path: output_file_path.clone(),
			},
			group: None,
		},
	);

	let mut processed_markdown_content = BufWriter::new(Vec::new());

	// Expecting panic here:
	liquid::process(
		&mut input_file,
		&mut processed_markdown_content,
		HashMap::new(),
		&liquid::Context {
			input_file_path: &input_file_path,
			output_file_path: &output_file_path,
			front_matter: &front_matter,
			html_content: None,
			root_input_dir: &PathBuf::from("./input"),
			root_output_dir: &PathBuf::from("./output"),
			input_output_map: &input_output_map,
			groups: &HashMap::new(),
			site_info: &SiteInfo { title: "Site" },
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
		make_front_matter("Title A", Some("2001-01-19T20:10:01Z"));
	let front_matter_b = make_front_matter("Title B", None);

	let mut input_file = BufReader::new(Cursor::new(
		(r#"{% for post in posts %}-{{ post.date }} <a href="{% link post.link %}">{{ post.title }}</a>-{% endfor %}"#).as_bytes(),
	));

	let mut input_output_map = HashMap::new();
	input_output_map.insert(
		input_file_path_a.clone(),
		GroupedOptionOutputFile {
			file: OptionOutputFile {
				front_matter: Some(front_matter_a.clone()),
				path: output_file_path_a.clone(),
			},
			group: None,
		},
	);
	input_output_map.insert(
		input_file_path_b.clone(),
		GroupedOptionOutputFile {
			file: OptionOutputFile {
				front_matter: Some(front_matter_b.clone()),
				path: output_file_path_b,
			},
			group: None,
		},
	);

	let mut groups = HashMap::new();
	groups.insert(
		"posts".to_string(),
		vec![
			InputFile {
				front_matter: front_matter_a.clone(),
				path: input_file_path_a.clone(),
			},
			InputFile {
				front_matter: front_matter_b,
				path: input_file_path_b,
			},
		],
	);

	let mut processed_markdown_content = BufWriter::new(Vec::new());
	liquid::process(
		&mut input_file,
		&mut processed_markdown_content,
		HashMap::new(),
		&liquid::Context {
			input_file_path: &input_file_path_a,
			output_file_path: &output_file_path_a,
			front_matter: &front_matter_a,
			html_content: None,
			root_input_dir: &PathBuf::from("./input"),
			root_output_dir: &PathBuf::from("./output"),
			input_output_map: &input_output_map,
			groups: &groups,
			site_info: &SiteInfo { title: "Site" },
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
fn test_liquid_date() {
	let input_file_path = PathBuf::from("./input/virtual_test.md");
	let output_file_path = PathBuf::from("./output/virtual_test.html");
	let front_matter = make_front_matter("Title", Some("2001-12-31T24:43:51Z"));
	let mut input_file = BufReader::new(Cursor::new(
		(r#"{{ page.date | date "%Y-%m-%dT%H:%M:%SZ %y %b %h %B" }}"#)
			.as_bytes(),
	));

	let mut input_output_map = HashMap::new();
	input_output_map.insert(
		input_file_path.clone(),
		GroupedOptionOutputFile {
			file: OptionOutputFile {
				front_matter: Some(front_matter.clone()),
				path: output_file_path.clone(),
			},
			group: None,
		},
	);

	let mut processed_markdown_content = BufWriter::new(Vec::new());
	liquid::process(
		&mut input_file,
		&mut processed_markdown_content,
		HashMap::new(),
		&liquid::Context {
			input_file_path: &input_file_path,
			output_file_path: &output_file_path,
			front_matter: &front_matter,
			html_content: None,
			root_input_dir: &PathBuf::from("./input"),
			root_output_dir: &PathBuf::from("./output"),
			input_output_map: &input_output_map,
			groups: &HashMap::new(),
			site_info: &SiteInfo { title: "Site" },
		},
	);

	assert_eq!(
		String::from_utf8_lossy(
			&processed_markdown_content.into_inner().unwrap()
		),
		"2001-12-31T24:43:51Z 01 Dec Dec December"
	);
}

#[test]
fn test_liquid_upcase() {
	let input_file_path = PathBuf::from("./input/virtual_test.md");
	let output_file_path = PathBuf::from("./output/virtual_test.html");
	let front_matter = make_front_matter("Title", None);
	let mut input_file = BufReader::new(Cursor::new(
		(r#"{{ page.title | upcase }}"#).as_bytes(),
	));

	let mut input_output_map = HashMap::new();
	input_output_map.insert(
		input_file_path.clone(),
		GroupedOptionOutputFile {
			file: OptionOutputFile {
				front_matter: Some(front_matter.clone()),
				path: output_file_path.clone(),
			},
			group: None,
		},
	);

	let mut processed_markdown_content = BufWriter::new(Vec::new());
	liquid::process(
		&mut input_file,
		&mut processed_markdown_content,
		HashMap::new(),
		&liquid::Context {
			input_file_path: &input_file_path,
			output_file_path: &output_file_path,
			front_matter: &front_matter,
			html_content: None,
			root_input_dir: &PathBuf::from("./input"),
			root_output_dir: &PathBuf::from("./output"),
			input_output_map: &input_output_map,
			groups: &HashMap::new(),
			site_info: &SiteInfo { title: "Site" },
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
	let front_matter = make_front_matter("Title", None);
	let mut input_file = BufReader::new(Cursor::new(
		(r#"{{ page.title | upcase | downcase }}"#).as_bytes(),
	));

	let mut input_output_map = HashMap::new();
	input_output_map.insert(
		input_file_path.clone(),
		GroupedOptionOutputFile {
			file: OptionOutputFile {
				front_matter: Some(front_matter.clone()),
				path: output_file_path.clone(),
			},
			group: None,
		},
	);

	let mut processed_markdown_content = BufWriter::new(Vec::new());
	liquid::process(
		&mut input_file,
		&mut processed_markdown_content,
		HashMap::new(),
		&liquid::Context {
			input_file_path: &input_file_path,
			output_file_path: &output_file_path,
			front_matter: &front_matter,
			html_content: None,
			root_input_dir: &PathBuf::from("./input"),
			root_output_dir: &PathBuf::from("./output"),
			input_output_map: &input_output_map,
			groups: &HashMap::new(),
			site_info: &SiteInfo { title: "Site" },
		},
	);

	assert_eq!(
		String::from_utf8_lossy(
			&processed_markdown_content.into_inner().unwrap()
		),
		"title"
	);
}

#[test]
fn test_liquid_if() {
	let input_file_path = PathBuf::from("./input/virtual_test.md");
	let output_file_path = PathBuf::from("./output/virtual_test.html");
	let front_matter = make_front_matter("Title", None);
	let mut input_file = BufReader::new(Cursor::new(
		(r#"{% if page.title == "Awesome Shoes" %}
These shoes are awesome!
{% endif %}{% if page.title == "Title" %}They're okay{% endif %}"#)
			.as_bytes(),
	));

	let mut input_output_map = HashMap::new();
	input_output_map.insert(
		input_file_path.clone(),
		GroupedOptionOutputFile {
			file: OptionOutputFile {
				front_matter: Some(front_matter.clone()),
				path: output_file_path.clone(),
			},
			group: None,
		},
	);

	let mut processed_markdown_content = BufWriter::new(Vec::new());
	liquid::process(
		&mut input_file,
		&mut processed_markdown_content,
		HashMap::new(),
		&liquid::Context {
			input_file_path: &input_file_path,
			output_file_path: &output_file_path,
			front_matter: &front_matter,
			html_content: None,
			root_input_dir: &PathBuf::from("./input"),
			root_output_dir: &PathBuf::from("./output"),
			input_output_map: &input_output_map,
			groups: &HashMap::new(),
			site_info: &SiteInfo { title: "Site" },
		},
	);

	assert_eq!(
		String::from_utf8_lossy(
			&processed_markdown_content.into_inner().unwrap()
		),
		"They're okay"
	);
}

#[test]
fn test_liquid_assign() {
	let input_file_path = PathBuf::from("./input/virtual_test.md");
	let output_file_path = PathBuf::from("./output/virtual_test.html");
	let front_matter = make_front_matter("Title", None);
	let mut input_file = BufReader::new(Cursor::new(
		(r#"{% assign foo = "Awesome Shoes" %}{% if foo == "Awesome Shoes" %}{% assign bar = "works" %}assign {{ bar }}{% endif %}"#)
			.as_bytes(),
	));

	let mut input_output_map = HashMap::new();
	input_output_map.insert(
		input_file_path.clone(),
		GroupedOptionOutputFile {
			file: OptionOutputFile {
				front_matter: Some(front_matter.clone()),
				path: output_file_path.clone(),
			},
			group: None,
		},
	);

	let mut processed_markdown_content = BufWriter::new(Vec::new());
	liquid::process(
		&mut input_file,
		&mut processed_markdown_content,
		HashMap::new(),
		&liquid::Context {
			input_file_path: &input_file_path,
			output_file_path: &output_file_path,
			front_matter: &front_matter,
			html_content: None,
			root_input_dir: &PathBuf::from("./input"),
			root_output_dir: &PathBuf::from("./output"),
			input_output_map: &input_output_map,
			groups: &HashMap::new(),
			site_info: &SiteInfo { title: "Site" },
		},
	);

	assert_eq!(
		String::from_utf8_lossy(
			&processed_markdown_content.into_inner().unwrap()
		),
		"assign works"
	);
}
