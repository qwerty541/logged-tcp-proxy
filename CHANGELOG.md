# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added

- The proxy now shuts down gracefully on Ctrl-C (SIGINT): it stops accepting connections, closes in-flight connections, and exits cleanly with status `0`.
- Added an integration test suite — in-crate relay tests (`src/tests.rs`) and a black-box test of the compiled binary (`scripts/integration_test.py`) — covering data relay, teardown, concurrency, error handling, Ctrl-C shutdown, and real HTTP/MODBUS exchanges.

### Changed

- The `--timeout` option is now opt-in and acts as a whole-connection idle timeout. By default the proxy applies no idle timeout — it relays until a peer closes the connection or the proxy is interrupted with Ctrl-C. When set, `--timeout <seconds>` closes the connection only after both directions have been idle for that long (activity in either direction resets the timer), so an active transfer is never interrupted. The value must be between 1 and 3153600000 seconds (about 100 years); out-of-range values are rejected at startup instead of being able to overflow the internal clock.
- Migrated the codebase to the Rust 2024 edition.
- Bumped MSRV from 1.70.0 to 1.85.1.
- Removed `lib.rs` and used modules directly in the binary to make the crate binary-only. This is intentional to prevent the crate from being used as a library and to clarify that it is only meant to be used as a command-line tool.
- Switched to the opt-in `include` property in `Cargo.toml` instead of the inclusive `exclude` property to prevent irrelevant files from being packaged.

### Fixed

- Fixed a defect where closing or half-closing a proxied connection could make the proxy spin at 100% CPU on one core; end-of-stream is now handled cleanly and the connection is torn down.
- A close on one side of a proxied connection is now forwarded to the other side (graceful shutdown), and each direction keeps relaying until it ends, so a response that is still arriving when the client finishes sending is no longer dropped.
- The proxy no longer panics and abruptly drops a client when it cannot reach the destination: the connect failure is logged, that client connection is closed cleanly, and the listener keeps serving other clients.
- The proxy no longer panics on a listener bind failure (for example when the address is already in use); it logs the error and exits with a non-zero status.

### Documentation

- Created security protocol document with instructions for reporting security vulnerabilities.
- Created basic contributing document with instructions for contributing to the project.
- Table of contents currently hidden by default.
- Restructured changelog for better match with [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format.
- Updated license badge to be clickable and link to the license file in the repository.
- Updated Rust version badge to be clickable.
- Added total lines count badge to the README file.
- Added COCOMO badge to the README file.
- Added crates.io downloads badge to the README file.
- Updated copyright year in license file to 2023-2026.
- Updated the README and CONTRIBUTING documents with the new MSRV and other relevant information.
- Created CLAUDE.md with codebase guidance for AI-assisted development.

### Dependencies

- Updated `tokio` from 1.32.0 to 1.52.3
- Updated `clap` from 4.4.6 to 4.6.1
- Updated `env_logger` from 0.10.0 to 0.11.10
- Updated `rustix` from 0.38.8 to 0.38.27
- Updated `logged-stream` from 0.3.4 to 0.6.0
- Updated `log` from 0.4.20 to 0.4.32
- Updated `mio` from 0.8.9 to 0.8.11
- Updated `bytes` from 1.5.0 to 1.11.1
- Updated `anstream` from 0.6.7 to 0.6.21
- Updated `slab` from 0.4.10 to 0.4.11

## v0.1.2 (08.10.2023)

### Added

- Added ability to provide custom bytes separator via command line argument `--separator`.
- Added ability to provide custom timestamp precision of console payload output via command line argument `--precision`.

### Changed

- Removed redundant module path and target from console output.
- Bump minimal supported rust version (MSRV) to 1.70.0.

### Dependencies

- Updated `bytes` from 1.4.0 to 1.5.0
- Updated `clap` from 4.2.5 to 4.4.6
- Updated `log` from 0.4.17 to 0.4.20
- Updated `logged-stream` from 0.2.5 to 0.3.4
- Updated `tokio` from 1.27.0 to 1.32.0
- Updated several indirect dependencies.

## v0.1.1 (08.05.2023)

- README improvements

## v0.1.0 (06.05.2023)

Initial release
