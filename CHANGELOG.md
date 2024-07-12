# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## v0.1.2 (08.10.2023)

-   Added ability to provide custom bytes separator via command line argument `--separator`.
-   Added ability to provide custom timestamp precision of console payload output via command line argument `--precision`.
-   Removed redundant module path and target from console output.
-   Bump minimal supported rust version (MSRV) to 1.70.0.
-   README improvements.
-   Dependencies updates:
    -   `bytes` from 1.4.0 to 1.5.0
    -   `clap` from 4.2.5 to 4.4.6
    -   `log` from 0.4.17 to 0.4.20
    -   `logged-stream` from 0.2.5 to 0.3.4
    -   `tokio` from 1.27.0 to 1.32.0
    -   Some indirect dependencies.

## v0.1.1 (08.05.2023)

-   README improvements

## v0.1.0 (06.05.2023)

Initial release
