use std::convert::TryInto;
use std::path::PathBuf;
use std::{env, fmt, fs};

use yaml_rust::YamlLoader;

use crate::util::SiteInfo;

pub struct BoolArg {
	pub name: &'static str,
	pub help: &'static str,
	pub value: bool,
	pub set: bool,
}

pub struct I16Arg {
	pub name: &'static str,
	pub help: &'static str,
	pub value: i16,
	pub set: bool,
}

pub struct StringArg {
	pub name: &'static str,
	pub help: &'static str,
	pub value: String,
	pub set: bool,
}

impl fmt::Display for BoolArg {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "-{} {}", self.name, self.help)
	}
}

impl fmt::Display for I16Arg {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "-{} {}", self.name, self.help)
	}
}

impl fmt::Display for StringArg {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "-{} {}", self.name, self.help)
	}
}

// Not using the otherwise brilliant CLAP crate since I detest string matching
// arg names by string across the code to get their values.
// Could use the structopt crate, but it unfortunately pulls in CLAP and 2 other
// dependencies (+2 dev-dependencies).
pub struct Args {
	pub author: StringArg,
	pub base_url: StringArg,
	pub deploy: BoolArg,
	pub email: StringArg,
	pub help: BoolArg, // Command line-only, doesn't transfer into Config.
	pub host: StringArg,
	pub input: StringArg,
	pub output: StringArg,
	pub port: I16Arg,
	pub serial: BoolArg,
	pub single_file: StringArg,
	pub title: StringArg,
	pub watch: BoolArg,
}

pub struct Config {
	pub author: String,
	pub base_url: String,
	pub deploy: bool,
	pub email: String,
	pub host: String,
	pub input_dir: PathBuf,
	pub output_dir: PathBuf,
	pub port: i16,
	pub serial: bool,
	pub single_file: Option<PathBuf>,
	pub title: String,
	pub watch: bool,
}

impl Args {
	pub fn new() -> Self {
		Self {
			author: StringArg {
				name: "author",
				help: "Set the name of the author.",
				value: String::from("John Doe"),
				set: false,
			},
			base_url: StringArg {
				name: "base_url",
				help: "Set base URL to be used in output files, default is \"http://<host>:<port>/\" but you probably want something like \"http://foo.com/\".",
				value: String::from("http://127.0.0.1:8090/"),
				set: false,
			},
			deploy: BoolArg {
				name: "deploy",
				help: "Deploy site excluding unpublished pages.",
				value: false,
				set: false,
			},
			email: StringArg {
				name: "email",
				help: "Set email of the author.",
				value: String::from("john.doe@test.com"),
				set: false,
			},
			help: BoolArg {
				name: "help",
				help: "Print this text.",
				value: false,
				set: false,
			},
			host: StringArg {
				name: "host",
				help: "Set address to bind to for built-in HTTP server. The default 127.0.0.1 can be used for privacy and 0.0.0.0 to give access to other machines.",
				value: String::from("127.0.0.1"),
				set: false,
			},
			input: StringArg {
				name: "input",
				help: "Set input directory to process.",
				value: String::from("./input"),
				set: false,
			},
			output: StringArg {
				name: "output",
				help: "Set output directory to write to.",
				value: String::from("./output"),
				set: false,
			},
			port: I16Arg {
				name: "port",
				help: "Set port to bind to for built-in HTTP server (default 8090).",
				value: 8090,
				set: false,
			},
			serial: BoolArg {
				name: "serial",
				help: "Run initial file processing in serial mode instead of concurrently.",
				value: false,
				set: false,
			},
			single_file: StringArg {
				name: "single_file",
				help: "Set to path to only process that one single file.",
				value: String::from(""),
				set: false,
			},
			title: StringArg {
				name: "title",
				help: "Title of the site.",
				value: String::from("Default Title"),
				set: false,
			},
			watch: BoolArg {
				name: "watch",
				help: "Run indefinitely, watching input directory for changes.",
				value: false,
				set: false,
			},
		}
	}

	pub fn parse(&mut self, args: env::Args) {
		{
			let bool_args = &mut [
				&mut self.deploy,
				&mut self.help,
				&mut self.serial,
				&mut self.watch,
			];
			let i16_args = &mut [&mut self.port];
			let string_args = &mut [
				&mut self.author,
				&mut self.base_url,
				&mut self.email,
				&mut self.host,
				&mut self.input,
				&mut self.output,
				&mut self.single_file,
				&mut self.title,
			];

			Self::parse_cli(args, bool_args, i16_args, string_args);

			let help_index = 1;
			assert_eq!(bool_args[help_index].name, "help");
			if bool_args[help_index].value {
				return;
			}

			let input_index = 4;
			assert_eq!(string_args[input_index].name, "input");
			let input_dir = PathBuf::from(&string_args[input_index].value);

			Self::parse_file(&input_dir, bool_args, i16_args, string_args);
		}

		if !self.watch.value
			&& !self.deploy.value
			&& (self.host.set || self.port.set)
		{
			panic!(
				"{} or {} arg set without {} or {} arg, so they have no use.",
				self.host.name,
				self.port.name,
				self.watch.name,
				self.deploy.name
			)
		}

		if self.deploy.value && self.watch.value {
			panic!("Can't have both deploy and watch mode active at the same time due to possibly changing published states and not all types of files being hot-reloadable.")
		}
	}

	fn parse_cli(
		args: env::Args,
		bool_args: &mut [&mut BoolArg],
		i16_args: &mut [&mut I16Arg],
		string_args: &mut [&mut StringArg],
	) {
		let mut first_arg = true;
		let mut previous_arg = None;
		'arg_loop: for mut arg in args {
			// Skip executable arg itself.
			if first_arg {
				first_arg = false;
				continue;
			}

			if let Some(prev) = previous_arg {
				for string_arg in &mut *string_args {
					if prev == string_arg.name {
						string_arg.value = arg;
						string_arg.set = true;
						previous_arg = None;
						continue 'arg_loop;
					}
				}

				for i16_arg in &mut *i16_args {
					if prev == i16_arg.name {
						i16_arg.value =
							arg.parse::<i16>().unwrap_or_else(|e| {
								panic!(
									"Invalid value for {}: {}",
									i16_arg.name, e
								);
							});
						i16_arg.set = true;
						previous_arg = None;
						continue 'arg_loop;
					}
				}

				panic!("Unhandled key-value arg: {}", prev);
			}

			if !arg.starts_with("--") {
				panic!("Unexpected argument: {}", arg)
			}

			arg = arg.split_off(2);

			for bool_arg in &mut *bool_args {
				if arg == bool_arg.name {
					bool_arg.value = true;
					bool_arg.set = true;
					continue 'arg_loop;
				}
			}

			for i16_arg in &*i16_args {
				if arg == i16_arg.name {
					previous_arg = Some(arg);
					continue 'arg_loop;
				}
			}

			for string_arg in &*string_args {
				if arg == string_arg.name {
					previous_arg = Some(arg);
					continue 'arg_loop;
				}
			}

			panic!("Unsupported argument: {}", arg)
		}
	}

	fn parse_file(
		input_dir: &PathBuf,
		bool_args: &mut [&mut BoolArg],
		i16_args: &mut [&mut I16Arg],
		string_args: &mut [&mut StringArg],
	) {
		let file_path = input_dir.join("_config.yml");
		if !file_path.exists() {
			return;
		}

		let contents = fs::read(&file_path).unwrap_or_else(|e| {
			panic!("Failed reading {}: {}", file_path.display(), e)
		});

		let yaml =
			YamlLoader::load_from_str(&String::from_utf8_lossy(&contents))
				.unwrap_or_else(|e| {
					panic!(
						"Failed loading YAML front matter from \"{}\": {}.",
						file_path.display(),
						e
					)
				});

		if yaml.len() != 1 {
			panic!("Expected only one YAML root element (Hash) in configuration file \"{}\" but got {}.", 
				file_path.display(), yaml.len());
		}

		if let yaml_rust::Yaml::Hash(hash) = &yaml[0] {
			let input_arg_name = "input";
			assert!(string_args.iter().any(|arg| arg.name == input_arg_name));
			let help_arg_name = "help";
			assert!(bool_args.iter().any(|arg| arg.name == help_arg_name));
			for (key, value) in hash {
				if let yaml_rust::Yaml::String(key) = key {
					if key == input_arg_name {
						panic!("Cannot override input through configuration file {}, can only be done on command line.", file_path.display())
					} else if key == help_arg_name {
						panic!("Cannot set help configuration file {}, can only be done on command line.", file_path.display())
					}

					Self::parse_yaml_attribute(
						key,
						value,
						&file_path,
						bool_args,
						i16_args,
						string_args,
					)
				} else {
					panic!("Expected string keys in YAML element in front matter of \"{}\" but got {:?}.", 
							file_path.display(), &key)
				}
			}
		} else {
			panic!("Expected Hash as YAML root element in front matter of \"{}\" but got {:?}.", 
				file_path.display(), &yaml[0])
		}
	}

	fn parse_yaml_attribute(
		key: &str,
		value: &yaml_rust::Yaml,
		file_path: &PathBuf,
		bool_args: &mut [&mut BoolArg],
		i16_args: &mut [&mut I16Arg],
		string_args: &mut [&mut StringArg],
	) {
		for arg in &mut *bool_args {
			if arg.name != key {
				continue;
			}
			if arg.set {
				return;
			}

			if let yaml_rust::Yaml::Boolean(value) = value {
				arg.value = *value;
				arg.set = true;
				return;
			} else {
				panic!(
					"{} in {} has unexpected type {:?}",
					key,
					file_path.display(),
					value
				)
			}
		}

		for arg in &mut *i16_args {
			if arg.name != key {
				continue;
			}
			if arg.set {
				return;
			}

			if let yaml_rust::Yaml::Integer(value) = value {
				arg.value =
					(*value).try_into().unwrap_or_else(|e| {
						panic!("Failed converting i64 to i16 for {} with value {} in {}: {}", key, value, file_path.display(), e)
					});
				arg.set = true;
				return;
			} else {
				panic!(
					"{} in {} has unexpected type {:?}",
					key,
					file_path.display(),
					value
				)
			}
		}

		for arg in &mut *string_args {
			if arg.name != key {
				continue;
			}
			if arg.set {
				return;
			}

			if let yaml_rust::Yaml::String(value) = value {
				arg.value = value.clone();
				arg.set = true;
				return;
			} else {
				panic!(
					"{} in {} has unexpected type {:?}",
					key,
					file_path.display(),
					value
				)
			}
		}

		panic!(
			"Unknown field {} in config file {}.",
			key,
			file_path.display(),
		)
	}

	pub fn print_help(&self) {
		println!("{}", self.author);
		println!("{}", self.base_url);
		println!("{}", self.deploy);
		println!("{}", self.email);
		println!("{}", self.help);
		println!("{}", self.host);
		println!("{}", self.input);
		println!("{}", self.output);
		println!("{}", self.port);
		println!("{}", self.serial);
		println!("{}", self.single_file);
		println!("{}", self.title);
		println!("{}", self.watch);
	}

	pub fn values(self) -> Config {
		let base_url = if self.base_url.set {
			self.base_url.value
		} else {
			format!("http://{}:{}/", &self.host.value, &self.port.value)
		};

		let single_file = if self.single_file.value.is_empty() {
			None
		} else {
			Some(PathBuf::from(self.single_file.value))
		};

		Config {
			author: self.author.value,
			base_url,
			deploy: self.deploy.value,
			email: self.email.value,
			host: self.host.value,
			input_dir: PathBuf::from(self.input.value),
			output_dir: PathBuf::from(self.output.value),
			port: self.port.value,
			serial: self.serial.value,
			single_file,
			title: self.title.value,
			watch: self.watch.value,
		}
	}
}

pub fn make_site_info(config: &Config) -> SiteInfo {
	SiteInfo {
		title: &config.title,
	}
}
