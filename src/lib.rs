pub mod command;
mod config;
mod graph;

use core::fmt;
use std::ffi::OsStr;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::{env, io, process};

use cargo_metadata::{Message, MetadataCommand};
use command::{Input, Test};
use config::Config;
use rustc_version::Channel;
use semver::Version;
use tee::TeeReader;

use crate::command::{CargoCmd, Run};
use crate::graph::UnitGraph;

/// Build a command using [`make_cargo_build_command`] and execute it,
/// parsing and returning the messages from the spawned process.
///
/// For commands that produce an executable output, this function will build the
/// `.elf` binary that can be used to create other nds files.
pub fn run_cargo(input: &Input, message_format: Option<String>) -> (ExitStatus, Vec<Message>) {
    let mut command = make_cargo_command(input, &message_format);

    if input.verbose {
        print_command(&command);
    }

    let mut process = command.spawn().unwrap();
    let command_stdout = process.stdout.take().unwrap();

    let mut tee_reader;
    let mut stdout_reader;

    let buf_reader: &mut dyn BufRead = match (message_format, &input.cmd) {
        // The user presumably cares about the message format if set, so we should
        // copy stuff to stdout like they expect. We can still extract the executable
        // information out of it that we need for ndstool etc.
        (Some(_), _) |
        // Rustdoc unfortunately prints to stdout for compile errors, so
        // we also use a tee when building doc tests too.
        // Possibly related: https://github.com/rust-lang/rust/issues/75135
        (None, CargoCmd::Test(Test { doc: true, .. })) => {
            tee_reader = BufReader::new(TeeReader::new(command_stdout, io::stdout()));
            &mut tee_reader
        }
        _ => {
            stdout_reader = BufReader::new(command_stdout);
            &mut stdout_reader
        }
    };

    let messages = Message::parse_stream(buf_reader)
        .collect::<io::Result<_>>()
        .unwrap();

    (process.wait().unwrap(), messages)
}

/// Create a cargo command based on the context.
///
/// For "build" commands (which compile code, such as `cargo nds build` or `cargo nds clippy`),
/// if there is no pre-built std detected in the sysroot, `build-std` will be used instead.
pub fn make_cargo_command(input: &Input, message_format: &Option<String>) -> Command {
    let blocksds =
        env::var("BLOCKSDS").unwrap_or("/opt/wonderful/thirdparty/blocksds/core".to_owned());
    let rustflags = format!("-C link-args=-specs={blocksds}/sys/crts/ds_arm9.specs");

    let cargo_cmd = &input.cmd;

    let mut command = cargo(&input.config);
    command
        .arg(cargo_cmd.subcommand_name())
        .env("RUSTFLAGS", rustflags);

    // Any command that needs to compile code will run under this environment.
    // Even `clippy` and `check` need this kind of context, so we'll just assume any other `Passthrough` command uses it too.
    if cargo_cmd.should_compile() {
        command
            .arg("--target")
            .arg("armv5te-nintendo-ds.json")
            .arg("-Z")
            .arg("build-std=core,alloc")
            .arg("--message-format")
            .arg(
                message_format
                    .as_deref()
                    .unwrap_or(CargoCmd::DEFAULT_MESSAGE_FORMAT),
            );
    }

    if let CargoCmd::Test(test) = cargo_cmd {
        // RUSTDOCFLAGS is simply ignored if --doc wasn't passed, so we always set it.
        let rustdoc_flags = std::env::var("RUSTDOCFLAGS").unwrap_or_default() + test.rustdocflags();
        command.env("RUSTDOCFLAGS", rustdoc_flags);
    }

    command.args(cargo_cmd.cargo_args());

    if let CargoCmd::Run(run) | CargoCmd::Test(Test { run_args: run, .. }) = &cargo_cmd {
        if run.use_custom_runner() {
            command
                .arg("--")
                .args(run.build_args.passthrough.exe_args());
        }
    }

    command
        .stdout(Stdio::piped())
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit());

    command
}

/// Build a `cargo` command with the given `--config` flags.
fn cargo(config: &[String]) -> Command {
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut cmd = Command::new(cargo);
    cmd.args(config.iter().map(|cfg| format!("--config={cfg}")));
    cmd
}

fn print_command(command: &Command) {
    let mut cmd_str = vec![command.get_program().to_string_lossy().to_string()];
    cmd_str.extend(command.get_args().map(|s| s.to_string_lossy().to_string()));

    eprintln!("Running command:");
    for (k, v) in command.get_envs() {
        let v = v.map(|v| v.to_string_lossy().to_string());
        eprintln!(
            "   {}={} \\",
            k.to_string_lossy(),
            v.map_or_else(String::new, |s| shlex::quote(&s).to_string())
        );
    }
    eprintln!("   {}\n", shlex::join(cmd_str.iter().map(String::as_str)));
}

/// Finds the sysroot path of the current toolchain
pub fn find_sysroot() -> PathBuf {
    let sysroot = env::var("SYSROOT").ok().unwrap_or_else(|| {
        let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());

        let output = Command::new(&rustc)
            .arg("--print")
            .arg("sysroot")
            .output()
            .unwrap_or_else(|_| panic!("Failed to run `{rustc} -- print sysroot`"));
        String::from_utf8(output.stdout).expect("Failed to parse sysroot path into a UTF-8 string")
    });

    PathBuf::from(sysroot.trim())
}

/// Checks the current rust version and channel.
/// Exits if the minimum requirement is not met.
pub fn check_rust_version() {
    let rustc_version = rustc_version::version_meta().unwrap();

    if rustc_version.channel > Channel::Nightly {
        eprintln!("cargo-nds requires a nightly rustc version.");
        eprintln!(
            "Please run `rustup override set nightly` to use nightly in the \
            current directory, or use `cargo +nightly nds` to use it for a \
            single invocation."
        );
        process::exit(1);
    }

    let old_version = MINIMUM_RUSTC_VERSION
        > Version {
            // Remove `-nightly` pre-release tag for comparison.
            pre: semver::Prerelease::EMPTY,
            ..rustc_version.semver.clone()
        };

    let old_commit = match rustc_version.commit_date {
        None => false,
        Some(date) => {
            MINIMUM_COMMIT_DATE
                > CommitDate::parse(&date).expect("could not parse `rustc --version` commit date")
        }
    };

    if old_version || old_commit {
        eprintln!("cargo-nds requires rustc nightly version >= {MINIMUM_COMMIT_DATE}");
        eprintln!("Please run `rustup update nightly` to upgrade your nightly version");

        process::exit(1);
    }
}

/// Parses messages returned by "build" cargo commands (such as `cargo nds build` or `cargo nds run`).
/// The returned [`CTRConfig`] is then used for further building in and execution
/// in [`build_nds`], and [`link`].
pub fn get_metadata(messages: &[Message]) -> NDSConfig {
    let metadata = MetadataCommand::new()
        .no_deps()
        .exec()
        .expect("Failed to get cargo metadata");

    let mut package = None;
    let mut artifact = None;

    // Extract the final built executable. We may want to fail in cases where
    // multiple executables, or none, were built?
    for message in messages.iter().rev() {
        if let Message::CompilerArtifact(art) = message {
            if art.executable.is_some() {
                package = Some(metadata[&art.package_id].clone());
                artifact = Some(art.clone());

                break;
            }
        }
    }
    if package.is_none() || artifact.is_none() {
        eprintln!("No executable found from build command output!");
        process::exit(1);
    }

    let (package, artifact) = (package.unwrap(), artifact.unwrap());

    let mut icon = String::from("./icon.bmp");

    if !Path::new(&icon).exists() {
        icon = format!("{}/sys/icon.bmp", env::var("BLOCKSDS").unwrap());
    }

    // for now assume a single "kind" since we only support one output artifact
    let name = match artifact.target.kind[0].as_ref() {
        "bin" | "lib" | "rlib" | "dylib" if artifact.target.test => {
            format!("{} tests", artifact.target.name)
        }
        "example" => {
            format!("{} - {} example", artifact.target.name, package.name)
        }
        _ => artifact.target.name,
    };

    let author = match package.authors.as_slice() {
        [name, ..] => name.clone(),
        [] => String::from("Unspecified Author"), // as standard with the devkitPRO toolchain
    };

    NDSConfig {
        name: name,
        author: author,
        description: package
            .description
            .clone()
            .unwrap_or_else(|| String::from("Homebrew Application")),
        icon: icon,
        target_path: artifact.executable.unwrap().into(),
        cargo_manifest_path: package.manifest_path.into(),
    }
}

/// Builds the nds using `ndstool`.
/// This will fail if `ndstool` is not within the running directory or in a directory found in $PATH
pub fn build_nds(config: &NDSConfig, verbose: bool) {
    let mut command = Command::new("ndstool");
    let name = get_name(config);

    let output_config = Config::try_load(config).expect("Failed to load nds.toml");

    let banner_text = if output_config.name.iter().any(|i| i.is_some()) {
        output_config
            .name
            .into_iter()
            .map(|i| i.unwrap_or_default())
            .collect::<Vec<String>>()
            .join(";")
    } else {
        format!(
            "{};{};{}",
            name.0.file_name().unwrap().to_string_lossy(),
            &config.description,
            &config.author
        )
    };

    let icon = get_icon_path(config);

    command
        .arg("-c")
        .arg(config.path_nds())
        .arg("-9")
        .arg(config.path_arm9())
        .arg("-7")
        .arg(config.path_arm7())
        .arg("-b")
        .arg(&icon)
        .arg(banner_text);

    // If romfs directory exists, automatically include it
    let (romfs_path, is_default_romfs) = get_romfs_path(config);
    if romfs_path.is_dir() {
        eprintln!("Adding RomFS from {}", romfs_path.display());
        command.arg("-d").arg(&romfs_path);
    } else if !is_default_romfs {
        eprintln!(
            "Could not find configured RomFS dir: {}",
            romfs_path.display()
        );
        process::exit(1);
    }

    if verbose {
        print_command(&command);
    }

    let mut process = command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("ndstool command failed, most likely due to 'ndstool' not being in $PATH");

    let status = process.wait().unwrap();

    if !status.success() {
        process::exit(status.code().unwrap_or(1));
    }
}

/// Link the generated nds to a ds to execute and test using `dslink`.
/// This will fail if `dslink` is not within the running directory or in a directory found in $PATH
pub fn link(config: &NDSConfig, run_args: &Run, verbose: bool) {
    let mut command = Command::new("dslink");
    command
        .args(run_args.get_dslink_args())
        .arg(config.path_nds())
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    if verbose {
        print_command(&command);
    }

    let status = command.spawn().unwrap().wait().unwrap();

    if !status.success() {
        process::exit(status.code().unwrap_or(1));
    }
}

/// Read the `RomFS` path from the Cargo manifest. If it's unset, use the default.
/// The returned boolean is true when the default is used.
pub fn get_romfs_path(config: &NDSConfig) -> (PathBuf, bool) {
    let manifest_path = &config.cargo_manifest_path;
    let manifest_str = std::fs::read_to_string(manifest_path)
        .unwrap_or_else(|e| panic!("Could not open {}: {e}", manifest_path.display()));
    let manifest_data: toml::Value =
        toml::de::from_str(&manifest_str).expect("Could not parse Cargo manifest as TOML");

    // Find the romfs setting and compute the path
    let mut is_default = false;
    let romfs_dir_setting = manifest_data
        .as_table()
        .and_then(|table| table.get("package"))
        .and_then(toml::Value::as_table)
        .and_then(|table| table.get("metadata"))
        .and_then(toml::Value::as_table)
        .and_then(|table| table.get("nds"))
        .and_then(toml::Value::as_table)
        .and_then(|table| table.get("romfs"))
        .and_then(toml::Value::as_str)
        .unwrap_or_else(|| {
            is_default = true;
            "romfs"
        });
    let mut romfs_path = manifest_path.clone();
    romfs_path.pop(); // Pop Cargo.toml
    romfs_path.push(romfs_dir_setting);

    (romfs_path, is_default)
}

/// Read the `RomFS` path from the Cargo manifest. If it's unset, use the default.
/// The returned boolean is true when the default is used.
pub fn get_name(config: &NDSConfig) -> (PathBuf, bool) {
    let manifest_path = &config.cargo_manifest_path;
    let manifest_str = std::fs::read_to_string(manifest_path)
        .unwrap_or_else(|e| panic!("Could not open {}: {e}", manifest_path.display()));
    let manifest_data: toml::Value =
        toml::de::from_str(&manifest_str).expect("Could not parse Cargo manifest as TOML");

    // Find the romfs setting and compute the path
    let mut is_default = false;
    let name_setting = manifest_data
        .as_table()
        .and_then(|table| table.get("package"))
        .and_then(toml::Value::as_table)
        .and_then(|table| table.get("name"))
        .and_then(toml::Value::as_str)
        .unwrap_or_else(|| {
            is_default = true;
            "No Name"
        });
    let mut name = manifest_path.clone();
    name.pop(); // Pop Cargo.toml
    name.push(name_setting);

    (name, is_default)
}

/// Read the `icon` path from the Cargo manifest. If it's unset, use the default.
/// The returned boolean is true when the default is used.
pub fn get_icon_path(config: &NDSConfig) -> PathBuf {
    let manifest_path = &config.cargo_manifest_path;

    let config = Config::try_load(config).expect("Failed to load nds.toml");
    match config.icon {
        Some(icon) => {
            let mut icon_path = manifest_path.clone();
            icon_path.pop(); // Pop Cargo.toml
            icon_path.push(icon);
            icon_path
        }
        None => "/opt/wonderful/thirdparty/blocksds/core/sys/icon.bmp".into(),
    }
}

#[derive(Default, Debug)]
pub struct NDSConfig {
    name: String,
    author: String,
    description: String,
    icon: String,
    target_path: PathBuf,
    cargo_manifest_path: PathBuf,
}

impl NDSConfig {
    pub fn path_nds(&self) -> PathBuf {
        self.target_path.with_extension("").with_extension("nds")
    }
    pub fn path_arm9(&self) -> PathBuf {
        self.target_path
            .with_extension("")
            .with_extension("arm9.elf")
    }
    pub fn path_arm7(&self) -> PathBuf {
        let arm7 = self
            .target_path
            .with_extension("")
            .with_extension("arm7.elf");
        if arm7.exists() {
            return arm7;
        }
        let blocksds =
            env::var("BLOCKSDS").unwrap_or("/opt/wonderful/thirdparty/blocksds/core".to_owned());
        PathBuf::from(format!("{}/sys/default_arm7/arm7.elf", blocksds))
    }
}

#[derive(Ord, PartialOrd, PartialEq, Eq, Debug)]
pub struct CommitDate {
    year: i32,
    month: i32,
    day: i32,
}

impl CommitDate {
    fn parse(date: &str) -> Option<Self> {
        let mut iter = date.split('-');

        let year = iter.next()?.parse().ok()?;
        let month = iter.next()?.parse().ok()?;
        let day = iter.next()?.parse().ok()?;

        Some(Self { year, month, day })
    }
}

impl fmt::Display for CommitDate {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

const MINIMUM_COMMIT_DATE: CommitDate = CommitDate {
    year: 2023,
    month: 5,
    day: 31,
};
const MINIMUM_RUSTC_VERSION: Version = Version::new(1, 70, 0);
