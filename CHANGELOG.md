# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Changed

- Updated minimal supported rust version (MSRV) from 1.70.0 to 1.74.1

### Documentation

- Table of contents currently hidden by default.
- Restructured changelog for better match with [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format.

### Dependencies

- Updated `tokio` from 1.32.0 to 1.38.0
- Updated `clap` from 4.4.6 to 4.5.8
- Updated `env_logger` from 0.10.0 to 0.10.2
- Updated `rustix` from 0.38.8 to 0.38.27
- Updated `logged-stream` from 0.3.4 to 0.4.0
- Updated `log` from 0.4.20 to 0.4.22
- Updated `mio` from 0.8.9 to 0.8.11
- Updated `bytes` from 1.5.0 to 1.6.0

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
