use std::path::PathBuf;
use std::{env, fmt};

pub struct BoolArg {
	pub name: &'static str,
	pub help: &'static str,
	pub value: bool,
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
// args to get their values.
pub struct ConfigArgs {
	pub author: StringArg,
	pub base_url: StringArg,
	pub email: StringArg,
	pub help: BoolArg, // Command line-only, doesn't transfer into Config.
	pub host: StringArg,
	pub input: StringArg,
	pub output: StringArg,
	pub port: I16Arg,
	pub watch: BoolArg,
}

pub struct Config {
	pub author: String,
	pub base_url: String,
	pub email: String,
	pub host: String,
	pub input_dir: PathBuf,
	pub output_dir: PathBuf,
	pub port: i16,
	pub watch: bool,
}

impl ConfigArgs {
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
				help: "Set base URL to be used in output files, default is \"http://test.com/\".",
				value: String::from("http://test.com/"),
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
			},
			host: StringArg {
				name: "host",
				help: "Set address to bind to. The default 127.0.0.1 can be used for privacy and 0.0.0.0 to give access to other machines.",
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
				help: "Set port to bind to.",
				value: 8090,
				set: false,
			},
			watch: BoolArg {
				name: "watch",
				help: "Run indefinitely, watching input directory for changes.",
				value: false,
			},
		}
	}

	pub fn parse(&mut self, args: env::Args) {
		let mut bool_args = vec![&mut self.help, &mut self.watch];
		let mut i16_args = vec![&mut self.port];
		let mut string_args = vec![
			&mut self.author,
			&mut self.base_url,
			&mut self.email,
			&mut self.host,
			&mut self.input,
			&mut self.output,
		];

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

			if arg.len() < 3
				|| arg.as_bytes()[0] != b'-'
				|| arg.as_bytes()[1] != b'-'
			{
				panic!("Unexpected argument: {}", arg)
			}

			arg = arg.split_off(2);

			for bool_arg in &mut *bool_args {
				if arg == bool_arg.name {
					bool_arg.value = true;
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

	pub fn print_help(&self) {
		println!("{}", self.author);
		println!("{}", self.base_url);
		println!("{}", self.email);
		println!("{}", self.help);
		println!("{}", self.host);
		println!("{}", self.input);
		println!("{}", self.output);
		println!("{}", self.port);
		println!("{}", self.watch);
	}

	pub fn values(self) -> Config {
		Config {
			author: self.author.value,
			base_url: self.base_url.value,
			email: self.email.value,
			host: self.host.value,
			input_dir: PathBuf::from(self.input.value),
			output_dir: PathBuf::from(self.output.value),
			port: self.port.value,
			watch: self.watch.value,
		}
	}
}
