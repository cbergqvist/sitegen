# sitegen

Generic static site generator written in Rust.

Features:

- Built-in local HTTP server with automated browser reload on save
- Multi-threaded generation of output files 
- Partial Liquid template language support (`assign`/`capture`/`if`/`else`/`for`/`include`/`link`)
- Generates Atom feeds, _robots.txt_ and _sitemap.xml_
- `--deploy` mode which avoids content marked as unpublished

## Convention over configuration

Putting Markdown files under _articles/_ will make the system default to the layout template file being _\_layout/article.html_. Putting them under _posts/_ will make it be _\_layout/post.html_.

Not setting the title in the front matter of Markdown files will try to grab it from the file name.

## Code quality

- Suffers from skirmishes with the borrow checker due to lack of Rust experience.
- Kept crate dependencies low in order to keep things nimble and less frail. Also providing more opportunity for experimentation and tuning (whether this is the right call depends on context).

## TODO

- Make deploy command verify state of _input/.git/_ is clean and pushed to remote. Should then use ftp or sftp to upload refreshed _output/_. Having it clean & pushed ensures that any links in the generated pages back to the remote git source file shows a version of the file that is at least as new.
- Add liquid function for outputting input url for reaching remote _input/_ git repo version of the current file. Maybe just have original _.md_-path support and have the git base url be a user defined variable.
- When running in dev webserver mode, serve files from variable-depth sub-directories (varying for each run) instead of directly under _/_ to help shake out absolute paths.
- Put posts under 4-number year directories for minimum order, both in input and output?
