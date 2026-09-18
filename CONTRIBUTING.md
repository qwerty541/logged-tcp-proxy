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
  - [Manual testing](#manual-testing)
  - [Linting \& Formatting](#linting--formatting)
  - [Testing](#testing)
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
- Minimum Supported Rust Version (MSRV): `1.85.1`
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
# Proxy from local 127.0.0.1:20502 to 127.0.0.1:20582 with lower-hex payload logging
cargo run -- \
  --bind-listener-addr 127.0.0.1:20502 \
  --remote-addr 127.0.0.1:20582 \
  --formatting lowerhex \
  --separator : \
  --precision seconds \
  --level debug

# Decimal formatting with millisecond timestamps
cargo run -- \
  --bind-listener-addr 127.0.0.1:20502 \
  --remote-addr 127.0.0.1:20582 \
  --formatting decimal \
  --separator , \
  --precision milliseconds
```

### Manual testing

To watch the proxy at work you need something on both sides of it.
[`scripts/peer.py`](scripts/peer.py) provides both: a scriptable **server** (the
destination) and a **client**. It uses only the Python standard library (3.8+), so
there is nothing to install. The default addresses match the proxy command below,
so three terminals are all you need:

```bash
# T1: the proxy, between the client (port 20502) and the server (port 20582)
cargo run -- -b 127.0.0.1:20502 -r 127.0.0.1:20582 -p milliseconds

# T2: the destination; by default it replies "re: " + whatever it receives
python3 scripts/peer.py server

# T3: the client; type a line to send it, /help lists the steps, Ctrl-D ends
python3 scripts/peer.py client
```

The peers log in the proxy's own format, so the three terminals can be read side by
side:

- `<` always means client → server bytes and `>` server → client bytes, in every
  terminal. Payloads are rendered with the same `-f`/`-s` options as the proxy (the
  peers accept them too).
- The client's tag `[c:PORT]` is the port in the proxy's
  `[#N] Incoming connection from 127.0.0.1:PORT` line. The server's `[s:PORT]` is the
  proxy's outgoing side.
- The peers also show what the proxy cannot: `-` FIN sent, `=` EOF received, `x`
  socket closed (and how), `!` socket error.

Besides typed input, both peers run scripted **steps**:
- sending: text with escapes (`hello\n`), raw bytes (`x:00:01:6f:ff`);
- pacing: `sleep:0.5`, `read`, `hold`;
- closing: `shut` (half-close), then an ending: `close` (graceful, the default), or
  `abort` / `rst` to close abruptly (`rst` always sends RST).

The client takes steps as arguments; the server takes them as `--on-accept` /
`--on-eof` hooks. `--split N --gap S` sends every payload in N-byte writes. See
`python3 scripts/peer.py client --help`.

Ready-made scenarios include banners, half-close, resets, the idle timeout,
`--max-connections`, a destination that is down, and a MODBUS poll:

```bash
python3 scripts/peer.py recipes             # list them
python3 scripts/peer.py recipes half-close  # the three commands, and what to look for
```

The deterministic recipes also run in CI as part of `scripts/integration_test.py`, so
the commands they print keep working.

Tips:

- Stop the proxy with Ctrl-C: it logs its shutdown line and exits with status 0.
  Closing its terminal or `kill` sends SIGTERM instead, which skips that.
- The client retries a refused connection for 15 s, so the terminals can be started
  in any order. It never opens a probe connection, so the first client is the
  proxy's `[#1]`.
- To merge the three logs into one timeline:
  1. Use the same `-p` everywhere; `microseconds` avoids ties.
  2. `tee` each terminal into a file. The proxy logs to stderr, so use
     `cargo run -- ... 2>&1 | tee proxy.log`.
  3. Run `sort proxy.log server.log client.log`.

### Linting & Formatting

- Formatting: `cargo fmt --all`
- Linting: `cargo clippy --all-targets --all-features -- -D warnings` (this is the
  command CI runs; `--all-targets` is what lints the in-crate `src/tests/` tree)

### Testing

Run the in-crate integration tests (they spin up their own servers on ephemeral
loopback ports, so no setup is required):

```bash
cargo test
```

There is also a black-box test that drives the **compiled binary** end to end. It
uses only the Python standard library (no `pip` packages). It also runs the
deterministic [manual-testing](#manual-testing) recipes, so run it after changing
`scripts/peer.py` or the proxy's console output:

```bash
python3 scripts/integration_test.py
```

Both run automatically in CI on every push and pull request.

## Project Structure

This is a **binary-only** crate — there is intentionally no `lib` target.

- `src/` — application source code
  - `args.rs` — CLI arguments, value enums, and payload formatter selection
  - `conn.rs` — TCP proxying core: accept loop, connection cap, bidirectional relay, logging, and idle timeout
  - `main.rs` — binary entry point, async runtime construction, and logger initialization
  - `tests.rs` + `tests/` — in-crate integration tests (compiled only under `#[cfg(test)]`), grouped into submodules by behavior; `tests/helpers.rs` holds the shared test helpers
- `scripts/integration_test.py` — black-box test that drives the compiled binary
- `scripts/peer.py` — manual-testing peers: a scriptable client and server to run around the proxy, plus ready-made scenarios (`recipes`)
- `Cargo.toml` — crate metadata (edition 2024, MSRV 1.85.1, licenses)
- `README.md` — usage, installation, and reference docs
- `CHANGELOG.md` — release notes
- `CONTRIBUTING.md` — this guide
- `RELEASE.md` — the release checklist
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
- Keep this guide current when a change touches what it documents — the project structure, the development commands, or the MSRV/edition

## Commit & PR Etiquette

- Use conventional, descriptive commit messages (e.g., `feat:`, `fix:`, `docs:`)
- Reference issues (e.g., `resolves #123`) when applicable
- Keep PRs focused; include notes on testing and potential impacts
- Before submitting, ensure locally:
  - `cargo fmt --all`
  - `cargo clippy --all-targets --all-features -- -D warnings`
  - `cargo test`
  - `python3 scripts/integration_test.py`, if you changed `scripts/` or the proxy's console output

## Security

Please report vulnerabilities via the documented security protocol.

- See [SECURITY.md](./SECURITY.md)

## License

By contributing, you agree that your contributions will be licensed under the terms listed in [LICENSE-APACHE](./LICENSE-APACHE) and [LICENSE-MIT](./LICENSE-MIT).
