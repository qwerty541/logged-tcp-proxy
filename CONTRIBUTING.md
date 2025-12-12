# Contributing to `logged-tcp-proxy`

Thanks for your interest in contributing! This document outlines how to propose changes, report issues, and develop locally. The project follows common practices used across the Rust crates community.

## Ground rules

- Be respectful and constructive. This project follows a Code of Conduct.
- Discuss large changes via issue before opening a PR.
- Keep PRs small and focused; separate unrelated changes.
- Ensure CI/build, lint, and tests pass locally before submitting.

## Code of Conduct

Please read and adhere to the [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).

## Security

If you believe you have found a security vulnerability, please do not open a public issue. Follow the instructions in [SECURITY.md](SECURITY.md).

## Licensing

By contributing, you agree that your contributions will be licensed under the terms of this repository: dual-licensed as [MIT](LICENSE-MIT) and [Apache-2.0](LICENSE-APACHE).

## Repository structure and crate info

- Crate name: `logged_tcp_proxy`
- Edition: 2021
- Minimum Supported Rust Version (MSRV): `1.74.1` (from `Cargo.toml`’s `rust-version`)
- Binary entrypoint: `src/main.rs`
- Library exports: `src/lib.rs`
- Core modules: `args.rs` (CLI args and formatters), `conn.rs` (connection handling)

## Getting started

### Prerequisites
- Rust toolchain via [rustup](https://rustup.rs/), MSRV `1.74.1` or newer.
- Recommended components:
  - `rustfmt` for formatting
  - `clippy` for linting

Install the components:

```bash
rustup component add rustfmt clippy
```

### Build and run

```bash
# Build
cargo build

# Run (example; adjust addresses as needed)
cargo run -- \
  --bind-listener-addr 127.0.0.1:9000 \
  --remote-addr 127.0.0.1:9100 \
  --formatting lowerhex \
  --separator : \
  --precision seconds \
  --level debug
```

### Formatting

Use Rustfmt’s default style. Before committing:

```bash
cargo fmt --all
```

### Linting

Use Clippy and fix warnings or justify them:

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

### Testing

Add unit tests where reasonable (e.g., argument parsing, formatter selection) and consider integration tests for proxy behavior.

```bash
cargo test --all
```

If adding async code/tests, prefer `tokio::test` for executor support.

### Docs

- Keep `README.md` in sync with behavior and CLI options.
- Public APIs (in `lib.rs`) should have doc comments. Run:

```bash
cargo doc --no-deps
```

## Development guidelines

- Follow the MSRV unless discussed otherwise. Use simple, well-supported APIs.
- Prefer small, cohesive functions. Avoid unnecessary unsafe.
- Log thoughtfully: use levels consistently (`trace` to `error`).
- Handle errors gracefully; avoid panics in normal flows. If `expect`/`unwrap` is used, justify in comments.
- Keep async boundaries clear; avoid blocking calls in async contexts.
- For CLI changes, update `Arguments` in `args.rs`, and ensure help texts and defaults are accurate.

## Commit messages

- Use clear, descriptive messages. Example prefix styles are welcome (e.g., `feat:`, `fix:`, `docs:`, `refactor:`), but not required.
- Reference issues when applicable (e.g., `fixes #123`).

## Branching and PRs

- Create a topic branch from the default branch.
- Describe the change in the PR body: motivation, approach, and trade-offs.
- Include a short checklist:
  - [ ] `cargo fmt` run
  - [ ] `cargo clippy` passes (no warnings)
  - [ ] `cargo test` passes
  - [ ] Docs updated (`README.md`, comments)
  - [ ] MSRV respected

## Local tips for this project

- Logging level is controlled by CLI argument `--level`; the app sets `RUST_LOG` accordingly.
- Payload formatting options: `decimal`, `lowerhex`, `upperhex`, `binary`, `octal`.
- Timestamp precision options: `seconds`, `milliseconds`, `microseconds`, `nanoseconds`.
- Connection proxy behavior is in `conn.rs`; consider adding tests around forwarding, timeouts, and shutdown behavior.

## Questions

If you’re unsure about an approach, open a discussion or an issue before large work.
