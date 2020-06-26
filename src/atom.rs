// Atom was chosen over RSS as the former has a saner date format.
use std::fs;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use crate::front_matter;
use crate::util::write_to_stream;

pub struct FeedHeader {
	pub title: String,
	pub base_url: String,
	pub latest_update: String,
	pub author_name: String,
	pub author_email: String,
}

pub struct FeedEntry {
	pub front_matter: front_matter::FrontMatter,
	pub html_content: String,
	pub permalink: PathBuf,
}

pub fn generate(
	file_path: &PathBuf,
	header: &FeedHeader,
	entries: Vec<FeedEntry>,
	output_dir: &PathBuf,
) {
	fn complete_url(base_url: &str, path: &str) -> String {
		let mut url = base_url.to_string();
		url.push_str(path);
		url
	}

	let parent_dir = file_path.parent().unwrap_or_else(|| {
		panic!(
			"Feed file path without a parent directory?: {}",
			file_path.display()
		)
	});
	fs::create_dir_all(parent_dir).unwrap_or_else(|e| {
		panic!(
			"Failed creating directories for {}: {}",
			parent_dir.display(),
			e
		)
	});

	let mut feed = fs::File::create(&file_path).unwrap_or_else(|e| {
		panic!("Failed creating {}: {}", file_path.display(), e)
	});

	let mut output = BufWriter::new(Vec::new());
	let feed_url = complete_url(
		&header.base_url,
		&file_path
			.strip_prefix(output_dir)
			.unwrap_or_else(|e| {
				panic!(
					"Failed stripping prefix {} from {}: {}",
					output_dir.display(),
					file_path.display(),
					e
				)
			})
			.to_string_lossy(),
	);
	write_to_stream(
		format!(
			"<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<feed xmlns=\"http://www.w3.org/2005/Atom\">
	<title>{}</title>
	<link rel=\"self\" href=\"{}\"/>
	<id>{}</id>
	<updated>{}</updated>
	<author>
		<name>{}</name>
		<email>{}</email>
	</author>

",
			header.title,
			feed_url,
			feed_url,
			header.latest_update,
			header.author_name,
			header.author_email
		)
		.as_bytes(),
		&mut output,
	);

	for entry in entries {
		let entry_url =
			complete_url(&header.base_url, &entry.permalink.to_string_lossy());
		write_to_stream(
			format!(
				"	<entry>
		<title>{}</title>
		<published>{}</published>
		<updated>{}</updated>
		<link href=\"{}\"/>
		<id>{}</id>
		<content type=\"html\"><![CDATA[
{}
]]></content>
	</entry>

",
				entry.front_matter.title,
				entry.front_matter.date,
				entry
					.front_matter
					.edited
					.unwrap_or_else(|| String::from("2001-01-19T20:10:00Z")),
				entry_url,
				entry_url,
				entry.html_content
			)
			.as_bytes(),
			&mut output,
		);
	}

	write_to_stream(b"</feed>", &mut output);

	feed.write_all(output.buffer()).unwrap_or_else(|e| {
		panic!("Failed writing to \"{}\": {}.", &file_path.display(), e)
	});

	// Avoiding sync_all() for now to be friendlier to disks.
	feed.sync_data().unwrap_or_else(|e| {
		panic!(
			"Failed sync_data() for \"{}\": {}.",
			&file_path.display(),
			e
		)
	});
}
