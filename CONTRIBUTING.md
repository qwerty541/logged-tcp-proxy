# Contributing to logged-tcp-proxy <!-- omit in toc -->

<details>
<summary>Table of contents</summary>

- [Description](#description)
- [Code of Conduct](#code-of-conduct)
- [Ways to Contribute](#ways-to-contribute)
- [Development Setup](#development-setup)
  - [Prerequisites](#prerequisites)
  - [Building](#building)
  - [Running](#running)
  - [Linting \& Formatting](#linting--formatting)
- [Project Structure](#project-structure)
- [Feature Guidelines](#feature-guidelines)
- [Performance \& Reliability](#performance--reliability)
- [Documentation](#documentation)
- [Commit \& PR Etiquette](#commit--pr-etiquette)
- [Security](#security)
- [License](#license)
</details>

## Description

Thanks for your interest in contributing! This document outlines how to propose changes, report issues, and develop locally. The project follows common practices used across the Rust crates community.

## Code of Conduct

This project adheres to a Code of Conduct. By participating, you agree to uphold it.

- See [CODE_OF_CONDUCT.md](./CODE_OF_CONDUCT.md)

## Ways to Contribute

- Report bugs or suggest improvements via GitHub Issues
- Implement features (check open issues or propose new ones)
- Improve documentation (README, CLI help, examples)
- Triage issues (labels, reproductions, platform checks)

## Development Setup

### Prerequisites

- Rust toolchain (stable) installed via [rustup](https://rustup.rs/)
- Cargo (bundled with rustup)
- Minimum Supported Rust Version (MSRV): `1.74.1`
- Recommended components:
  - `rustfmt` for formatting
  - `clippy` for linting

Install recommended components:

```bash
rustup component add rustfmt clippy
```

### Building

```bash
cargo build
```

### Running

```bash
cargo run -- [OPTIONS]
```

Common quick runs:

```bash
# Proxy from local 127.0.0.1:9000 to 127.0.0.1:9100 with lower-hex payload logging
cargo run -- \
  --bind-listener-addr 127.0.0.1:9000 \
  --remote-addr 127.0.0.1:9100 \
  --formatting lowerhex \
  --separator : \
  --precision seconds \
  --level debug

# Decimal formatting with millisecond timestamps
cargo run -- \
  --bind-listener-addr 127.0.0.1:8000 \
  --remote-addr 127.0.0.1:8080 \
  --formatting decimal \
  --separator , \
  --precision milliseconds \
  --level info
```

### Linting & Formatting

- Formatting: `cargo fmt --all`
- Linting: `cargo clippy --all-targets --all-features -- -D warnings`

## Project Structure

- `src/` — application source code
  - `args.rs` — CLI arguments, value enums, and payload formatter selection
  - `conn.rs` — TCP proxying logic, logging, timeouts, and task management
  - `lib.rs` — re-exports for library consumers
  - `main.rs` — binary entry point and logger initialization
- `Cargo.toml` — crate metadata (edition 2021, MSRV 1.74.1, licenses)
- `README.md` — usage, installation, and reference docs
- `CHANGELOG.md` — release notes
- `SECURITY.md` — how to report security issues
- `CODE_OF_CONDUCT.md` — community standards
- `LICENSE-APACHE`, `LICENSE-MIT` — dual license files

## Feature Guidelines

- Keep default behavior sensible: safe logging defaults, reasonable buffer sizes, and clear timeouts
- Add flags for opt-in changes rather than breaking existing behavior
- Maintain consistent output formatting across supported kinds (decimal/lowerhex/upperhex/binary/octal)
- Prefer small, incremental PRs

## Performance & Reliability

- Avoid blocking operations in async contexts; the project uses Tokio and `LoggedStream`
- Re-use buffers (`BytesMut`) where possible and avoid unnecessary allocations
- Use timeouts thoughtfully; ensure tasks are cancelled cleanly on shutdown or errors
- Be cautious with spawn/abort semantics; prefer structured concurrency where feasible
- Use appropriate log levels; avoid excessive logging in hot paths

## Documentation

- Update [README.md](./README.md) and examples when adding or changing CLI arguments
- Add changelog entries under `## Unreleased` and reference commits/issues in [CHANGELOG.md](./CHANGELOG.md)

## Commit & PR Etiquette

- Use conventional, descriptive commit messages (e.g., `feat:`, `fix:`, `docs:`)
- Reference issues (e.g., `resolves #123`) when applicable
- Keep PRs focused; include notes on testing and potential impacts
- Before submitting, ensure locally:
  - `cargo fmt --all`
  - `cargo clippy --all-targets --all-features -- -D warnings`

## Security

Please report vulnerabilities via the documented security protocol.

- See [SECURITY.md](./SECURITY.md)

## License

By contributing, you agree that your contributions will be licensed under the terms listed in [LICENSE-APACHE](./LICENSE-APACHE) and [LICENSE-MIT](./LICENSE-MIT).
