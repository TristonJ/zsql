# ZSQL
[![CI](https://github.com/TristonJ/zsql/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/TristonJ/zsql/actions/workflows/ci.yml?query=branch%3Amain) [![codecov](https://codecov.io/gh/TristonJ/zsql/branch/main/graph/badge.svg)](https://codecov.io/gh/TristonJ/zsql) [![License: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-blue.svg)](https://github.com/TristonJ/zsql/blob/main/LICENSE)

A lightweight developer-first SQL editor.

## Screenshots

<details>
<summary>Basic Query Editor</summary>

<p align="center"><img src="./docs/screenshot_simple.jpg" width="800"/></p>

</details>

<details>
<summary>Schema Explorer</summary>

<p align="center"><img src="./docs/screenshot_schema.jpg" width="800"/></p>
</details>

## Features
- Lightweight (around ~40mb installed) - built with Zed's [gpui](https://docs.rs/gpui/latest/gpui/)
- Multi-database support
- Simple query generation (with filters, pagination, and sorting)
- Simple schema exploration (table & column metadata)
- Custom SQL scripts with syntax highlighting
- Detailed value viewer (JSON, hex, etc.)
- Multiple theme & custom theme support
- SQL script library per-connection

## Database Support
- PostgreSQL
- MySQL
- SQLite
- Microsoft SQL Server

## Installing

Depending on your platform, there are a few ways to install ZSQL.

### AppImage

On Linux, you can download the latest AppImage from the [releases](
https://github.com/TristonJ/zsql/releases) page. The AppImage is a portable
Linux application that can be run without installation.

### Debian Package

On debian-based distributions (e.g. Ubuntu), you can download the latest deb
package from the [releases](https://github.com/TristonJ/zsql/releases) page.

### Arch Linux

On Arch Linux based distributions, there is currently a PKGBUILD available on
the [releases](https://github.com/TristonJ/zsql/releases) page. To install from
that PKGBUILD:

```sh
# Download the PKGBUILD & the tarball
curl -LO https://github.com/TristonJ/zsql/releases/download/v0.1.0/zsql-0.1.0-x86_64.tar.gz
curl -LO https://github.com/TristonJ/zsql/releases/download/v0.1.0/PKGBUILD

# Run makepkg to build the package
makepkg -si
```

> Note: AUR distribution coming soon.

### MacOS

On MacOS, you can download the latest `.dmg` from the [releases](
https://github.com/TristonJ/zsql/releases) page for your architecture (Intel or Apple Silicon).

Because there is _currently_ no developer certificate registered with Apple, you may get a
"app is damanged" warning when trying to open the app. To bypass this, you can execute
the following command in your terminal after installation:
```sh
sudo xattr -rd com.apple.quarantine /path/to/zsql.app
```

### From Source

Installing the binary from source is simple if you have Rust + Cargo installed:
```sh
cargo install --path crates/zsql
```

## Configuration

There are a number of configuration options available, including fonts, UI positioning,
query limits, etc. These are stored in your platform-specific configuration directory.
On Linux, this is typically `~/.config/zsql/config.toml`. On MacOS, this is typically
`~/Library/Application Support/zsql/config.toml`.

To see the full list of configuration options, see the `Config` struct in
`./crates/zsql/src/config.rs`. For example, to set the theme and fonts:
```toml
[theme]
name = "zsql-dark"

[theme.fonts]
data = "JetBrains Mono"
ui = "IBM Plex Sans"
```

## Building
The core binary is in `crates/zsql`. To build it in release mode:
```sh
cargo build --release
```

## Toolchain
- Latest stable Rust. Verify with `rustc --version` / `cargo --version`.
- `rustfmt` and `clippy` components: `rustup component add rustfmt clippy`.

## Development
Just like building, the main binary can be run with cargo:
```sh
cargo run 
```

See the [CONVENTIONS.md](./CONVENTIONS.md) for general coding conventions and guidelines.

### Local Databases
There are scripts for spinning up local development databases using docker in `./scripts`. For 
example:
```sh
./scripts/pg-dev.sh up      # ephemeral Postgres in Docker + seed data
./scripts/pg-dev.sh down    # stop it
```

### Testing
There is extensive unit testing across all of the crates. To run all tests, excluding the tests
that require running databases, run:
```sh
cargo test --all
```

If you're working on a driver, or would like to run the full test suite, you'll need to enable
the `driver-integration-tests` feature. There is a convenience script for running the full test
suite, including the integration tests that require locally running databases.
```sh
# Keep in mind - you'll need docker to actually spin up the databases
./scripts/test-all.sh
```

### Formatting & Linting
The project uses `rustfmt` and `clippy` (pedantic) for formatting and linting. To run these checks,
you can run:
```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
```
