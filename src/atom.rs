// Atom was chosen over RSS as the former has a saner date format.
use std::collections::HashMap;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use crate::front_matter;
use crate::util::write_to_stream;

pub struct FeedHeader {
	pub title: String,
	pub base_url: String,
	pub latest_update: Option<String>,
	pub author_name: String,
	pub author_email: String,
}

pub struct FeedEntry {
	pub front_matter: front_matter::FrontMatter,
	pub html_content: String,
	pub permalink: PathBuf,
}

pub fn generate(
	groups: &HashMap<String, Vec<FeedEntry>>,
	output_dir: &PathBuf,
	base_url: &str,
	author: &str,
	email: &str,
) {
	for (group, entries) in groups {
		let mut latest_update: Option<&String> = None;
		for entry in entries {
			if let Some(date) = &entry.front_matter.date {
				if let Some(latest) = latest_update {
					if latest < date {
						latest_update = Some(date);
					}
				} else {
					latest_update = Some(date)
				}
			}
			if let Some(date) = &entry.front_matter.edited {
				if let Some(latest) = latest_update {
					if latest < date {
						latest_update = Some(date);
					}
				} else {
					latest_update = Some(date)
				}
			}
		}

		let feed_name = output_dir
			.join(PathBuf::from("feeds").join(PathBuf::from(&group)))
			.with_extension("xml");
		let header = FeedHeader {
			title: group.to_string(),
			base_url: base_url.to_string(),
			latest_update: latest_update.cloned(),
			author_name: author.to_string(),
			author_email: email.to_string(),
		};

		generate_feed(&feed_name, &header, entries, &output_dir);
	}
}

fn generate_feed(
	file_path: &PathBuf,
	header: &FeedHeader,
	entries: &[FeedEntry],
	output_dir: &PathBuf,
) {
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
",
			header.title, feed_url, feed_url,
		)
		.as_bytes(),
		&mut output,
	);

	if let Some(latest_update) = &header.latest_update {
		write_to_stream(
			format!(
				"	<updated>{}</updated>
",
				latest_update,
			)
			.as_bytes(),
			&mut output,
		);
	}

	write_to_stream(
		format!(
			"	<author>
		<name>{}</name>
		<email>{}</email>
	</author>

",
			header.author_name, header.author_email
		)
		.as_bytes(),
		&mut output,
	);

	for entry in entries {
		generate_entry(&entry, header, &mut output);
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

fn generate_entry(
	entry: &FeedEntry,
	header: &FeedHeader,
	mut output: &mut BufWriter<Vec<u8>>,
) {
	let entry_url =
		complete_url(&header.base_url, &entry.permalink.to_string_lossy());

	write_to_stream(
		format!(
			"	<entry>
		<title>{}</title>
		<id>{}</id>
		<link href=\"{}\"/>
",
			entry.front_matter.title, entry_url, entry_url
		)
		.as_bytes(),
		&mut output,
	);

	if let Some(published_date) = &entry.front_matter.date {
		write_to_stream(
			format!(
				"		<published>{}</published>
",
				published_date
			)
			.as_bytes(),
			&mut output,
		);
	}
	if let Some(updated_date) = &entry.front_matter.edited {
		write_to_stream(
			format!(
				"		<updated>{}</updated>
",
				updated_date
			)
			.as_bytes(),
			&mut output,
		);
	}

	write_to_stream(
		format!(
			"		<content type=\"html\"><![CDATA[
{}
]]></content>
	</entry>

",
			entry.html_content
		)
		.as_bytes(),
		&mut output,
	);
}

fn complete_url(base_url: &str, path: &str) -> String {
	let mut url = base_url.to_string();
	url.push_str(path);
	url
}
