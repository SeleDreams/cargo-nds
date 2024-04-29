# cargo-nds

Cargo command to work with Nintendo DS project binaries. Based on cargo-3ds (https://github.com/rust3ds/cargo-3ds).

## Installation

To install the current `master` version of `cargo-nds`:

```sh
cargo install --git https://github.com/SeleDreams/cargo-nds.git
```
Before attempting to use it, make sure you installed the BlocksDS toolchain !

Follow the installation instructions available here : https://blocksds.github.io/docs/setup/options/

You will need to set the WONDERFUL_TOOLCHAIN and BLOCKSDS environment variables and have the directory of arm-none-eabi-gcc as well as ndstool in your PATH

arm-none-eabi-gcc is located at $WONDERFUL_TOOLCHAIN/toolchain/gcc-arm-none-eabi/bin

ndstool is located at $BLOCKSDS/tools/ndstool

to use ndslink, please check this repository https://github.com/devkitPro/install-dsilink 

## Usage

Use the nightly toolchain to build DS apps (either by using `rustup override nightly` for the project directory or by adding `+nightly` in the `cargo` invocation).

```txt
Commands:
  build
          Builds an executable suitable to run on a DS (nds)
  run
          Builds an executable and sends it to a device with `dslink`
  test
          Builds a test executable and sends it to a device with `dslink`
  new
          Sets up a new cargo project suitable to run on a DS
  help
          Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help information (use `-h` for a summary)

  -V, --version
          Print version information
```

Additional arguments will be passed through to the given subcommand.
See [passthrough arguments](#passthrough-arguments) for more details.

It is also possible to pass any other `cargo` command (e.g. `doc`, `check`),
and all its arguments will be passed through directly to `cargo` unmodified,
with the proper `--target armv5te-nintendo-ds.json` set.

### Basic Examples

* `cargo nds build`
* `cargo nds check --verbose`
* `cargo nds run --release --example foo`
* `cargo nds test --no-run`
* `cargo nds new my-new-project --edition 2021`
* `cargo nds init .`
### Running executables

`cargo nds test` and `cargo nds run` use the `dslink` tool to send built
executables to a device.

### Caveats

Due to the fact that only one executable at a time can be sent with `dslink`,
by default only the "last" executable built will be used. If a `test` or `run`
command builds more than one binary, you may need to filter it in order to run
the executable you want.

Doc tests sort of work, but `cargo-nds` uses a number of unstable cargo and
rustdoc features to make them work, so the output won't be as pretty and will
require some manual workarounds to actually run the tests and see output from them.
For now, `cargo nds test --doc` will not build a nds file or use `dslink` at all.

For the time being, only arm9 homebrews can be built. I am still thinking about the best way to integrate arm7 support to the workflow.

The default arm7 binary of blocksds will be bundled in the nds file.

## License

This project is distributed under the MIT license or the Apache-2.0 license.
