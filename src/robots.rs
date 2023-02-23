use std::cmp::Ordering;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use crate::markdown::GroupedOptionOutputFile;
use crate::util;
use crate::util::{strip_prefix, write_to_stream};

pub fn write_robots_txt(output_dir: &Path, sitemap_url: &str) {
	let file_name = output_dir.join(PathBuf::from("robots.txt"));
	let mut file = fs::File::create(&file_name).unwrap_or_else(|e| {
		panic!("Failed creating {}: {}", file_name.display(), e)
	});
	file.write_all(
		format!(
			"User-agent: *
Allow: /
Sitemap: {}
",
			sitemap_url
		)
		.as_bytes(),
	)
	.unwrap_or_else(|e| {
		panic!("Failed writing to {}: {}", file_name.display(), e)
	});
	// Avoiding sync_all() for now to be friendlier to disks.
	file.sync_data().unwrap_or_else(|e| {
		panic!("Failed sync_data() for \"{}\": {}.", file_name.display(), e)
	});
	println!("Wrote {}.", file_name.display());
}

pub fn write_sitemap_xml(
	output_dir: &Path,
	base_url: &str,
	input_output_map: &HashMap<PathBuf, GroupedOptionOutputFile>,
) -> String {
	struct Entry<'a> {
		path: String,
		date: Option<&'a str>,
	}

	let official_file_name = PathBuf::from("sitemap.xml");
	let file_name = output_dir.join(&official_file_name);
	let mut file = fs::File::create(&file_name).unwrap_or_else(|e| {
		panic!("Failed creating {}: {}", file_name.display(), e)
	});
	write_to_stream(
		b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n",
		&mut file,
	);

	let html_extension = OsStr::new(util::HTML_EXTENSION);

	let mut entries = Vec::new();
	for output_file in input_output_map.values() {
		if output_file.file.path.extension() != Some(html_extension) {
			continue;
		}

		let path = strip_prefix(&output_file.file.path, output_dir);
		let mut output_url = base_url.to_string();
		if path.file_name() == Some(OsStr::new("index.html")) {
			output_url.push_str(&path.with_file_name("").to_string_lossy())
		} else {
			output_url.push_str(&path.to_string_lossy())
		}

		let date_entry =
			if let Some(front_matter) = &output_file.file.front_matter {
				if let Some(date) = &front_matter.edited {
					Some(date.as_str())
				} else { front_matter.date.as_deref() }
			} else {
				None
			};
		entries.push(Entry {
			path: output_url,
			date: date_entry,
		});
	}

	entries.sort_by(|lhs, rhs| {
		let date_ordering = rhs.date.cmp(&lhs.date);
		match date_ordering {
			Ordering::Less | Ordering::Greater => date_ordering,
			Ordering::Equal => lhs.path.cmp(&rhs.path),
		}
	});

	for entry in &entries {
		write_to_stream(
			format!(
				"	<url>
		<loc>{}</loc>\n",
				entry.path
			)
			.as_bytes(),
			&mut file,
		);

		if let Some(date) = entry.date {
			write_to_stream(
				format!("		<lastmod>{}</lastmod>\n", date).as_bytes(),
				&mut file,
			);
		}

		write_to_stream(b"	</url>\n", &mut file);
	}

	write_to_stream(b"</urlset>\n", &mut file);
	// Avoiding sync_all() for now to be friendlier to disks.
	file.sync_data().unwrap_or_else(|e| {
		panic!("Failed sync_data() for \"{}\": {}.", file_name.display(), e)
	});
	println!("Wrote {}.", file_name.display());

	let mut result = base_url.to_string();
	result.push_str(&official_file_name.to_string_lossy());
	result
}
