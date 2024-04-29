use std::{env, fs};
use std::io::Read;
use std::process::Stdio;
use std::sync::OnceLock;

use cargo_metadata::Message;
use clap::{Args, Parser, Subcommand};

use crate::{build_nds, cargo, get_metadata, link, print_command, NDSConfig};

#[derive(Parser, Debug)]
#[command(name = "cargo", bin_name = "cargo")]
pub enum Cargo {
    #[command(name = "nds")]
    Input(Input),
}

#[derive(Args, Debug)]
#[command(version, about)]
pub struct Input {
    #[command(subcommand)]
    pub cmd: CargoCmd,

    /// Print the exact commands `cargo-nds` is running. Note that this does not
    /// set the verbose flag for cargo itself. To set cargo's verbosity flag, add
    /// `-- -v` to the end of the command line.
    #[arg(long, short = 'v', global = true)]
    pub verbose: bool,

    /// Set cargo configuration on the command line. This is equivalent to
    /// cargo's `--config` option.
    #[arg(long, global = true)]
    pub config: Vec<String>,
}

/// Run a cargo command. COMMAND will be forwarded to the real
/// `cargo` with the appropriate arguments for the nds target.
///
/// If an unrecognized COMMAND is used, it will be passed through unmodified
/// to `cargo` with the appropriate flags set for the nds target.
#[derive(Subcommand, Debug)]
#[command(allow_external_subcommands = true)]
pub enum CargoCmd {
    /// Builds an executable suitable to run on a DS (nds).
    Build(Build),

    /// Builds an executable and sends it to a device with `dslink`.
    Run(Run),

    /// Builds a test executable and sends it to a device with `dslink`.
    ///
    /// This can be used with `--test` for integration tests, or `--lib` for
    /// unit tests (which require a custom test runner).
    Test(Test),

    /// Sets up a new cargo project suitable to run on a DS.
    New(New),

    Init(Init),

    // NOTE: it seems docstring + name for external subcommands are not rendered
    // in help, but we might as well set them here in case a future version of clap
    // does include them in help text.
    /// Run any other `cargo` command with custom building tailored for the nds.
    #[command(external_subcommand, name = "COMMAND")]
    Passthrough(Vec<String>),
}

#[derive(Args, Debug)]
pub struct RemainingArgs {
    /// Pass additional options through to the `cargo` command.
    ///
    /// All arguments after the first `--`, or starting with the first unrecognized
    /// option, will be passed through to `cargo` unmodified.
    ///
    /// To pass arguments to an executable being run, a *second* `--` must be
    /// used to disambiguate cargo arguments from executable arguments.
    /// For example, `cargo nds run -- -- xyz` runs an executable with the argument
    /// `xyz`.
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "CARGO_ARGS"
    )]
    args: Vec<String>,
}

#[derive(Args, Debug)]
pub struct Build {
    #[arg(from_global)]
    pub verbose: bool,

    // Passthrough cargo options.
    #[command(flatten)]
    pub passthrough: RemainingArgs,
}

#[derive(Args, Debug)]
pub struct Run {
    /// Specify the IP address of the device to send the executable to.
    ///
    /// Corresponds to ndslink's `--address` arg, which defaults to automatically
    /// finding the device.
    #[arg(long, short = 'a')]
    pub address: Option<std::net::Ipv4Addr>,

    /// Set the 0th argument of the executable when running it. Corresponds to
    /// ndslink's `--argv0` argument.
    #[arg(long, short = '0')]
    pub argv0: Option<String>,

    /// Start the ndslink server after sending the executable. Corresponds to
    /// ndslink's `--server` argument.
    #[arg(long, short = 's', default_value_t = false)]
    pub server: bool,

    /// Set the number of tries when connecting to the device to send the executable.
    /// Corresponds to ndslink's `--retries` argument.
    // Can't use `short = 'r'` because that would conflict with cargo's `--release/-r`
    #[arg(long)]
    pub retries: Option<usize>,

    // Passthrough `cargo build` options.
    #[command(flatten)]
    pub build_args: Build,

    #[arg(from_global)]
    config: Vec<String>,
}

#[derive(Args, Debug)]
pub struct Test {
    /// If set, the built executable will not be sent to the device to run it.
    #[arg(long)]
    pub no_run: bool,

    /// If set, documentation tests will be built instead of unit tests.
    /// This implies `--no-run`, unless Cargo's `target.armv6k-nintendo-nds.runner`
    /// is configured.
    #[arg(long)]
    pub doc: bool,

    // The test command uses a superset of the same arguments as Run.
    #[command(flatten)]
    pub run_args: Run,
}

#[derive(Args, Debug)]
pub struct New {
    /// Path of the new project.
    #[arg(required = true)]
    pub path: String,

    // The test command uses a superset of the same arguments as Run.
    #[command(flatten)]
    pub cargo_args: RemainingArgs,
}

#[derive(Args, Debug)]
pub struct Init {
    /// Path of the new project.
    #[arg(required = false)]
    pub path: String,

    // The test command uses a superset of the same arguments as Run.
    #[command(flatten)]
    pub cargo_args: RemainingArgs,
}

impl CargoCmd {
    /// Returns the additional arguments run by the "official" cargo subcommand.
    pub fn cargo_args(&self) -> Vec<String> {
        match self {
            CargoCmd::Build(build) =>build.passthrough.cargo_args(),
            CargoCmd::Run(run) => run.build_args.passthrough.cargo_args(),
            CargoCmd::Test(test) => test.cargo_args(),
            CargoCmd::New(new) => {
                // We push the original path in the new command (we captured it in [`New`] to learn about the context)
                let mut cargo_args = new.cargo_args.cargo_args();
                cargo_args.push(new.path.clone());

                cargo_args
            },
            CargoCmd::Init(init) => {
                // We push the original path in the init command (we captured it in [`Init`] to learn about the context)
                let mut cargo_args = init.cargo_args.cargo_args();
                cargo_args.push(init.path.clone());

                cargo_args
            },
            CargoCmd::Passthrough(other) => other.clone().split_off(1),
        }
    }

    /// Returns the cargo subcommand run by `cargo-nds` when handling a [`CargoCmd`].
    ///
    /// # Notes
    ///
    /// This is not equivalent to the lowercase name of the [`CargoCmd`] variant.
    /// Commands may use different commands under the hood to function (e.g. [`CargoCmd::Run`] uses `build`
    /// if no custom runner is configured).
    pub fn subcommand_name(&self) -> &str {
        match self {
            CargoCmd::Build(_) => "build",
            CargoCmd::Run(run) => {
                if run.use_custom_runner() {
                    "run"
                } else {
                    "build"
                }
            }
            CargoCmd::Test(_) => "test",
            CargoCmd::New(_) => "new",
            CargoCmd::Init(_) => "init",
            CargoCmd::Passthrough(cmd) => &cmd[0],
        }
    }

    /// Whether or not this command should compile any code, and thus needs import the custom environment configuration (e.g. target spec).
    pub fn should_compile(&self) -> bool {
        matches!(
            self,
            Self::Build(_) | Self::Run(_) | Self::Test(_) | Self::Passthrough(_)
        )
    }

    /// Whether or not this command should build a ndsX executable file.
    pub fn should_build_ndsx(&self) -> bool {
        match self {
            Self::Build(_) | CargoCmd::Run(_) => true,
            &Self::Test(Test { doc, .. }) => {
                if doc {
                    eprintln!("Documentation tests requested, no ndsx will be built");
                    false
                } else {
                    true
                }
            }
            _ => false,
        }
    }

    /// Whether or not the resulting executable should be sent to the nds with
    /// `ndslink`.
    pub fn should_link_to_device(&self) -> bool {
        match self {
            Self::Test(Test { no_run: true, .. }) => false,
            Self::Run(run) | Self::Test(Test { run_args: run, .. }) => !run.use_custom_runner(),
            _ => false,
        }
    }

    pub const DEFAULT_MESSAGE_FORMAT: &'static str = "json-render-diagnostics";

    pub fn extract_message_format(&mut self) -> Result<Option<String>, String> {
        let cargo_args = match self {
            Self::Build(build) => &mut build.passthrough.args,
            Self::Run(run) => &mut run.build_args.passthrough.args,
            Self::New(new) => &mut new.cargo_args.args,
            Self::Init(init) => &mut init.cargo_args.args,
            Self::Test(test) => &mut test.run_args.build_args.passthrough.args,
            Self::Passthrough(args) => args,
        };

        let format = Self::extract_message_format_from_args(cargo_args)?;
        if format.is_some() {
            return Ok(format);
        }

        if let Self::Test(Test { doc: true, .. }) = self {
            // We don't care about JSON output for doctests since we're not
            // building any ndsx etc. Just use the default output as it's more
            // readable compared to DEFAULT_MESSAGE_FORMAT
            Ok(Some(String::from("human")))
        } else {
            Ok(None)
        }
    }

    fn extract_message_format_from_args(
        cargo_args: &mut Vec<String>,
    ) -> Result<Option<String>, String> {
        // Checks for a position within the args where '--message-format' is located
        if let Some(pos) = cargo_args
            .iter()
            .position(|s| s.starts_with("--message-format"))
        {
            // Remove the arg from list so we don't pass anything twice by accident
            let arg = cargo_args.remove(pos);

            // Allows for usage of '--message-format=<format>' and also using space separation.
            // Check for a '=' delimiter and use the second half of the split as the format,
            // otherwise remove next arg which is now at the same position as the original flag.
            let format = if let Some((_, format)) = arg.split_once('=') {
                format.to_string()
            } else {
                // Also need to remove the argument to the --message-format option
                cargo_args.remove(pos)
            };

            // Non-json formats are not supported so the executable exits.
            if format.starts_with("json") {
                Ok(Some(format))
            } else {
                Err(String::from(
                    "error: non-JSON `message-format` is not supported",
                ))
            }
        } else {
            Ok(None)
        }
    }

    /// Runs the custom callback *after* the cargo command, depending on the type of command launched.
    ///
    /// # Examples
    ///
    /// - `cargo nds build` and other "build" commands will use their callbacks to build the final `.ndsx` file and link it.
    /// - `cargo nds new` and other generic commands will use their callbacks to make nds-specific changes to the environment.
    pub fn run_callback(&self, messages: &[Message]) {
        // Process the metadata only for commands that have it/use it
        let config = if self.should_build_ndsx() {
            eprintln!("Getting metadata");

            Some(get_metadata(messages))
        } else {
            None
        };

        // Run callback only for commands that use it
        match self {
            Self::Build(cmd) => cmd.callback(&config),
            Self::Run(cmd) => cmd.callback(&config),
            Self::Test(cmd) => cmd.callback(&config),
            Self::New(cmd) => cmd.callback(),
            Self::Init(cmd) => cmd.callback(),
            _ => (),
        }
    }
}

impl RemainingArgs {
    /// Get the args to be passed to `cargo`.
    pub fn cargo_args(&self) -> Vec<String> {
        self.split_args().0
    }

    /// Get the args to be passed to the executable itself (not `cargo`).
    pub fn exe_args(&self) -> Vec<String> {
        self.split_args().1
    }

    fn split_args(&self) -> (Vec<String>, Vec<String>) {
        let mut args = self.args.clone();

        if let Some(split) = args.iter().position(|s| s == "--") {
            let second_half = args.split_off(split + 1);
            // take off the "--" arg we found, we'll add one later if needed
            args.pop();

            (args, second_half)
        } else {
            (args, Vec::new())
        }
    }
}

impl Build {
    /// Callback for `cargo nds build`.
    ///
    /// This callback handles building the application as a `.ndsx` file.
    fn callback(&self, config: &Option<NDSConfig>) {
        if let Some(config) = config {
            eprintln!("Building nds: {}", config.path_nds().display());
            build_nds(config, self.verbose);
        }
    }
}

impl Run {
    /// Get the args to pass to `ndslink` based on these options.
    pub fn get_dslink_args(&self) -> Vec<String> {
        let mut args = Vec::new();

        if let Some(address) = self.address {
            args.extend(["-a".to_string(), address.to_string()]);
        }

        args
    }

    /// Callback for `cargo nds run`.
    ///
    /// This callback handles launching the application via `dslink`.
    fn callback(&self, config: &Option<NDSConfig>) {
        // Run the normal "build" callback
        self.build_args.callback(config);

        if !self.use_custom_runner() {
            if let Some(cfg) = config {
                eprintln!("Running dslink");
                link(cfg, self, self.build_args.verbose);
            }
        }
    }

    /// Returns whether the cargo environment has `target.armv6k-nintendo-nds.runner`
    /// configured. This will only be checked once during the lifetime of the program,
    /// and takes into account the usual ways Cargo looks for its
    /// [configuration](https://doc.rust-lang.org/cargo/reference/config.html):
    ///
    /// - `.cargo/config.toml`
    /// - Environment variables
    /// - Command-line `--config` overrides
    pub fn use_custom_runner(&self) -> bool {
        static HAS_RUNNER: OnceLock<bool> = OnceLock::new();

        let &custom_runner_configured = HAS_RUNNER.get_or_init(|| {
            let blocksds = env::var("BLOCKSDS").unwrap_or("/opt/wonderful/thirdparty/blocksds/core".to_owned());
            env::set_var("RUSTFLAGS", format!("-C link-args=-specs={blocksds}/sys/crts/ds_arm9.specs"));

            let mut cmd = cargo(&self.config);
            cmd.args([
                // https://github.com/rust-lang/cargo/issues/9301
                "-Z",
                "build-std=core,alloc",
                "--target",
                "./armv5te-nintendo-ds.json"
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null());

            if self.build_args.verbose {
                print_command(&cmd);
            }

            // `cargo config get` exits zero if the config exists, or nonzero otherwise
            cmd.status().map_or(false, |status| status.success())
        });

        if self.build_args.verbose {
            eprintln!(
                "Custom runner is {}configured",
                if custom_runner_configured { "" } else { "not " }
            );
        }

        custom_runner_configured
    }
}

impl Test {
    /// Callback for `cargo nds test`.
    ///
    /// This callback handles launching the application via `ndslink`.
    fn callback(&self, config: &Option<NDSConfig>) {
        if self.no_run {
            // If the tests don't have to run, use the "build" callback
            self.run_args.build_args.callback(config);
        } else {
            // If the tests have to run, use the "run" callback
            self.run_args.callback(config);
        }
    }

    fn should_run(&self) -> bool {
        self.run_args.use_custom_runner() && !self.no_run
    }

    /// The args to pass to the underlying `cargo test` command.
    fn cargo_args(&self) -> Vec<String> {
        let mut cargo_args = self.run_args.build_args.passthrough.cargo_args();

        // We can't run nds executables on the host, but we want to respect
        // the user's "runner" configuration if set.
        //
        // If doctests were requested, `--no-run` will be rejected on the
        // command line and must be set with RUSTDOCFLAGS instead:
        // https://github.com/rust-lang/rust/issues/87022

        if self.doc {
            cargo_args.extend([
                "--doc".into(),
                // https://github.com/rust-lang/cargo/issues/7040
                "-Z".into(),
                "doctest-xcompile".into(),
            ]);
        } else if !self.should_run() {
            cargo_args.push("--no-run".into());
        }

        cargo_args
    }

    /// Flags to pass to rustdoc via RUSTDOCFLAGS
    pub(crate) fn rustdocflags(&self) -> &'static str {
        if self.should_run() {
            ""
        } else {
            // We don't support running doctests by default, but cargo doesn't like
            // --no-run for doctests, so we have to plumb it in via RUSTDOCFLAGS
            " --no-run"
        }
    }
}

const TOML_CHANGES: &str = r#"libnds-sys = { git = "https://github.com/SeleDreams/libnds-sys.git" }

[package.metadata.nds]
romfs_dir = "romfs"
"#;

const TARGET_JSON: &str = r#"{
    "abi": "eabi",
    "arch": "arm",
    "data-layout": "e-m:e-p:32:32-Fi8-i64:64-v128:64:128-a:0:32-n32-S64",
    "env" : "picolibc",
    "exe-suffix" : ".arm9.elf",
    "is-builtin": false,
    "linker": "arm-none-eabi-gcc",
    "llvm-target": "armv5te-none-gnu",
    "relocation-model": "static",
    "target-endian": "little",
    "target-pointer-width": "32",
    "target-c-int-width": "32",
    "executables": true,
    "linker-flavor": "gcc",
    "max-atomic-width": 32,
    "disable-redzone": true,
    "emit-debug-gdb-scripts": false,
    "features" : "+soft-float,+strict-align,+atomics-32",
    "panic-strategy" : "abort",
    "linker-is-gnu": true,
    "target-family": [
        "unix"
      ],
    "no-default-libraries": false,
    "main-needs-argc-argv":"false",
    "pre-link-args": {
        "gcc": [
          "--data-sections",
          "-march=armv5te",
          "-mthumb",
          "-mcpu=arm946e-s+nofp",
          "-mthumb-interwork",
          "-Wl,-Map,target/arm9.map",
          "-Wl,--gc-sections"
        ]
      },
      "post-link-args" : {
        "gcc": [
          "-Wl,--no-warn-rwx-segments",
          "-Wl,--allow-multiple-definition"
        ]
      },
      "late-link-args": {
        "gcc": [
            "-lgcc"
        ]
    },
    "vendor" : "nintendo",
    "os" : "nintendo_ds_arm9"
  }  
"#;

const CUSTOM_MAIN_RS: &str = r#"#![no_main]
#![no_std]
use core::ffi::c_int;
use libnds_sys::arm9_bindings::*;
#[no_mangle]
extern "C" fn main() -> c_int
{
    unsafe
    {
        consoleDemoInit();       
        printf("Hello World!\n\0".as_ptr() as *const i8);
        loop {
            swiWaitForVBlank();
            scanKeys();
            let keys = keysHeld();
            if (keys & KEY_START) > 0
            {
                break;
            }
        }
    }
    return 0;
}
"#;

const CUSTOM_CARGO_CONFIG : &str = r#"[profile.release]
codegen-units = 1
opt-level=3
debug-assertions=false
strip = "debuginfo"
lto = true
overflow-checks=false

[profile.dev]
codegen-units = 1
debug=2
opt-level=3
debug-assertions=false
lto = true
overflow-checks=false
strip = false
"#;

impl New {
    /// Callback for `cargo nds new`.
    ///
    /// This callback handles the custom environment modifications when creating a new nds project.
    fn callback(&self) {
        // Commmit changes to the project only if is meant to be a binary
        if self.cargo_args.args.contains(&"--lib".to_string()) {
            return;
        }

        // Attain a canonicalised path for the new project and it's TOML manifest
        let project_path = fs::canonicalize(&self.path).unwrap();
        let toml_path = project_path.join("Cargo.toml");
        let romfs_path = project_path.join("romfs");
        let main_rs_path = project_path.join("src/main.rs");
        let target_json_path = project_path.join("armv5te-nintendo-ds.json");
        let config_path = project_path.join(".cargo/config.toml");
        
        // Create the "romfs" directory
        fs::create_dir(romfs_path).unwrap();

        // Read the contents of `Cargo.toml` to a string
        let mut buf = String::new();
        fs::File::open(&toml_path)
            .unwrap()
            .read_to_string(&mut buf)
            .unwrap();

        // Add the custom changes to the TOML
        let buf = buf + TOML_CHANGES;
        fs::write(&toml_path, buf).unwrap();

        // Add the custom changes to the main.rs file
        fs::write(main_rs_path, CUSTOM_MAIN_RS).unwrap();

        fs::write(target_json_path,TARGET_JSON).unwrap();
        fs::create_dir(project_path.join(".cargo")).unwrap();
        fs::write(config_path, CUSTOM_CARGO_CONFIG).unwrap();

    }
}


impl Init {
    /// Callback for `cargo nds new`.
    ///
    /// This callback handles the custom environment modifications when creating a new nds project.
    fn callback(&self) {
        // Commmit changes to the project only if is meant to be a binary
        if self.cargo_args.args.contains(&"--lib".to_string()) {
            return;
        }

        // Attain a canonicalised path for the new project and it's TOML manifest
        let project_path = fs::canonicalize(&self.path).unwrap();
        let toml_path = project_path.join("Cargo.toml");
        let romfs_path = project_path.join("romfs");
        let main_rs_path = project_path.join("src/main.rs");
        let target_json_path = project_path.join("armv5te-nintendo-ds.json");
        let config_path = project_path.join(".cargo/config.toml");
        
        // Create the "romfs" directory
        fs::create_dir(romfs_path).unwrap();

        // Read the contents of `Cargo.toml` to a string
        let mut buf = String::new();
        fs::File::open(&toml_path)
            .unwrap()
            .read_to_string(&mut buf)
            .unwrap();

        // Add the custom changes to the TOML
        let buf = buf + TOML_CHANGES;
        fs::write(&toml_path, buf).unwrap();

        // Add the custom changes to the main.rs file
        fs::write(main_rs_path, CUSTOM_MAIN_RS).unwrap();

        fs::write(target_json_path,TARGET_JSON).unwrap();
        fs::create_dir(project_path.join(".cargo")).unwrap();
        fs::write(config_path, CUSTOM_CARGO_CONFIG).unwrap();

    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn verify_app() {
        Cargo::command().debug_assert();
    }

    #[test]
    fn extract_format() {
        const CASES: &[(&[&str], Option<&str>)] = &[
            (&["--foo", "--message-format=json", "bar"], Some("json")),
            (&["--foo", "--message-format", "json", "bar"], Some("json")),
            (
                &[
                    "--foo",
                    "--message-format",
                    "json-render-diagnostics",
                    "bar",
                ],
                Some("json-render-diagnostics"),
            ),
            (
                &["--foo", "--message-format=json-render-diagnostics", "bar"],
                Some("json-render-diagnostics"),
            ),
            (&["--foo", "bar"], None),
        ];

        for (args, expected) in CASES {
            let mut cmd = CargoCmd::Build(Build {
                passthrough: RemainingArgs {
                    args: args.iter().map(ToString::to_string).collect(),
                },
                verbose: false,
            });

            assert_eq!(
                cmd.extract_message_format().unwrap(),
                expected.map(ToString::to_string)
            );

            if let CargoCmd::Build(build) = cmd {
                assert_eq!(build.passthrough.args, vec!["--foo", "bar"]);
            } else {
                unreachable!();
            }
        }
    }

    #[test]
    fn extract_format_err() {
        for args in [&["--message-format=foo"][..], &["--message-format", "foo"]] {
            let mut cmd = CargoCmd::Build(Build {
                passthrough: RemainingArgs {
                    args: args.iter().map(ToString::to_string).collect(),
                },
                verbose: false,
            });

            assert!(cmd.extract_message_format().is_err());
        }
    }

    #[test]
    fn split_run_args() {
        struct TestParam {
            input: &'static [&'static str],
            expected_cargo: &'static [&'static str],
            expected_exe: &'static [&'static str],
        }

        for param in [
            TestParam {
                input: &["--example", "hello-world", "--no-default-features"],
                expected_cargo: &["--example", "hello-world", "--no-default-features"],
                expected_exe: &[],
            },
            TestParam {
                input: &["--example", "hello-world", "--", "--do-stuff", "foo"],
                expected_cargo: &["--example", "hello-world"],
                expected_exe: &["--do-stuff", "foo"],
            },
            TestParam {
                input: &["--lib", "--", "foo"],
                expected_cargo: &["--lib"],
                expected_exe: &["foo"],
            },
            TestParam {
                input: &["foo", "--", "bar"],
                expected_cargo: &["foo"],
                expected_exe: &["bar"],
            },
        ] {
            let input: Vec<&str> = ["cargo", "nds", "run"]
                .iter()
                .chain(param.input)
                .copied()
                .collect();

            dbg!(&input);
            let Cargo::Input(Input {
                cmd: CargoCmd::Run(Run { build_args, .. }),
                ..
            }) = Cargo::try_parse_from(input).unwrap_or_else(|e| panic!("{e}"))
            else {
                panic!("parsed as something other than `run` subcommand")
            };

            assert_eq!(build_args.passthrough.cargo_args(), param.expected_cargo);
            assert_eq!(build_args.passthrough.exe_args(), param.expected_exe);
        }
    }
}
