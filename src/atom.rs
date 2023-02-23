// Atom was chosen over RSS as the former has a saner date format.
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;
use std::io::BufWriter;
use std::path::PathBuf;
use std::sync::Arc;

use crate::front_matter;
use crate::util;
use crate::util::{strip_prefix, write_to_stream};

pub struct FeedHeader {
	pub title: String,
	pub base_url: String,
	pub latest_update: Option<String>,
	pub author_name: String,
	pub author_email: String,
}

pub struct FeedEntry {
	pub front_matter: Arc<front_matter::FrontMatter>,
	pub html_content: String,
	pub permalink: PathBuf,
}

pub fn generate(
	groups: HashMap<String, Vec<FeedEntry>>,
	output_dir: &PathBuf,
	base_url: &str,
	author: &str,
	email: &str,
	title: &str,
) {
	for (group, mut entries) in groups {
		entries.sort_by(|lhs, rhs| {
			let date_ordering =
				rhs.front_matter.date.cmp(&lhs.front_matter.date);
			match date_ordering {
				Ordering::Less | Ordering::Greater => date_ordering,
				Ordering::Equal => lhs.permalink.cmp(&rhs.permalink),
			}
		});

		// Since entries has already been sorted by now, we just need to find
		// the first date.
		let mut latest_update: Option<&String> = None;
		for entry in &entries {
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
			.with_extension(util::XML_EXTENSION);

		let header = FeedHeader {
			title: format!("{} - {}", title, util::capitalize(&group)),
			base_url: base_url.to_string(),
			latest_update: latest_update.cloned(),
			author_name: author.to_string(),
			author_email: email.to_string(),
		};

		generate_feed(&feed_name, &header, &entries, output_dir);
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

	let feed = fs::File::create(file_path).unwrap_or_else(|e| {
		panic!("Failed creating {}: {}", file_path.display(), e)
	});

	let mut output = BufWriter::new(feed);
	let feed_url = complete_url(
		&header.base_url,
		&strip_prefix(file_path, output_dir).to_string_lossy(),
	);
	write_to_stream(
		format!(
			"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
			<feed xmlns=\"http://www.w3.org/2005/Atom\">\n\
			\t<title>{}</title>\n\
			\t<link rel=\"self\" href=\"{}\"/>\n\
			\t<id>{}</id>\n",
			header.title, feed_url, feed_url,
		)
		.as_bytes(),
		&mut output,
	);

	if let Some(latest_update) = &header.latest_update {
		write_to_stream(
			format!("\t<updated>{}</updated>\n", latest_update,).as_bytes(),
			&mut output,
		);
	}

	write_to_stream(
		format!(
			"\t<author>\n\
			\t\t<name>{}</name>\n\
			\t\t<email>{}</email>\n\
			\t</author>\n",
			header.author_name, header.author_email
		)
		.as_bytes(),
		&mut output,
	);

	for entry in entries {
		generate_entry(entry, header, &mut output);
	}

	write_to_stream(b"</feed>", &mut output);

	let feed = output.into_inner().unwrap_or_else(|e| {
		panic!(
			"Failed flushing buffered data to feed \"{}\": {}.",
			&file_path.display(),
			e
		)
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
	mut output: &mut BufWriter<fs::File>,
) {
	let entry_url =
		complete_url(&header.base_url, &entry.permalink.to_string_lossy());

	write_to_stream(
		format!(
			"\n\
			\t<entry>\n\
			\t\t<title>{}</title>\n\
			\t\t<id>{}</id>\n\
			\t\t<link href=\"{}\"/>\n",
			entry.front_matter.title, entry_url, entry_url
		)
		.as_bytes(),
		&mut output,
	);

	if let Some(published_date) = &entry.front_matter.date {
		write_to_stream(
			format!("\t\t<published>{}</published>\n", published_date)
				.as_bytes(),
			&mut output,
		);
	}
	if let Some(updated_date) = &entry.front_matter.edited {
		write_to_stream(
			format!("\t\t<updated>{}</updated>\n", updated_date).as_bytes(),
			&mut output,
		);
	}

	write_to_stream(
		format!(
			"\t\t<content type=\"html\"><![CDATA[\
			{}\
			]]></content>\n\
			\t</entry>\n",
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
