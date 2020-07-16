# sitegen

## Convention over configuration

Putting Markdown files under `articles/` will make the system default to the layout template file being `_layout/article.html`. Putting them under `posts/` will make it be `_layout/post.html`.

## TODO

- Make deploy command verify state of input/.git/ is clean and pushed to remote. Should then use ftp or sftp to upload refreshed output/. Having it clean & pushed ensures that any links in the generated pages back to the remote git source file shows a version of the file that is at least as new.
- Add liquid function for outputting input url for reaching remote input/ git repo version of the current file. Maybe just have original .md-path support and have the git base url be a user defined variable.
- When running in dev webserver mode, serve files from variable-depth sub-directories (varying for each run) instead of directly under / to help shake out absolute paths.
- Put posts under 4-number year directories for minimum order, both in input and output?
