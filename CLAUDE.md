# CLAUDE.md

Guidance for working in this repository.

## What this project is

`logged_tcp_proxy` is a small command-line TCP proxy. It listens on a local
address, accepts incoming connections, opens a connection to a configured remote
("destination") address, and relays bytes in both directions while **printing
the payload to the console** in a chosen numeric format (lowercase/uppercase
hexadecimal, decimal, octal, or binary). It was originally written to debug
MODBUS/TCP traffic and is published as an open-source CLI on crates.io.

The payload logging is the whole point: the binary is a transparent
man-in-the-middle you put between a client (e.g. a MODBUS device or master) and a
server so you can watch the raw bytes of the conversation.

## Crate facts

- Package name / binary: `logged_tcp_proxy` (a **binary-only** crate — there is
  intentionally no `lib` target, so nothing is published as a library on
  docs.rs).
- Edition: `2024`. MSRV (`rust-version`): `1.85.1` (edition 2024 requires
  rustc ≥ 1.85).
- License: `MIT OR Apache-2.0`.
- Repository: https://github.com/qwerty541/logged-tcp-proxy

## Source layout

All source lives in `src/`:

- [`src/main.rs`](src/main.rs) — entry point. Starts a Tokio runtime
  (`#[tokio::main(flavor = "multi_thread", worker_threads = 4)]`), parses CLI
  arguments, configures `env_logger` (level, timestamp precision, no target/
  module path), and calls `conn::initialize_tcp_listener`.
- [`src/args.rs`](src/args.rs) — the `clap`-derived `Arguments` struct (all
  fields `pub`) plus three CLI value enums and their `ValueEnum` / `FromStr` /
  `Display` impls:
  - `LoggingLevel` → converts into `log::LevelFilter`.
  - `PayloadFormatingKind` → selects a `logged_stream` formatter via
    `get_formatter_by_kind`.
  - `TimestampPrecision` → converts into `env_logger`'s timestamp precision.
- [`src/conn.rs`](src/conn.rs) — the networking core:
  - `initialize_tcp_listener(arguments)` (`pub`) — binds the `TcpListener`,
    logs that it is ready, then runs the accept loop.
  - `run_accept_loop(listener, arguments)` (`pub(crate)`) — the accept loop:
    for each accepted connection it logs the peer address and spawns
    `incoming_connection_handle`. Extracted from `initialize_tcp_listener` so it
    can be driven by tests with a pre-bound (ephemeral-port) listener.
  - `incoming_connection_handle(arguments, source_stream)` (private) — sets up
    the per-connection bidirectional relay (see below).
  - `relay(reader, writer, read_timeout)` (private, generic) — copies bytes in
    one direction until end-of-stream, a read/write error, or (when
    `read_timeout` is set) an idle read timeout, then shuts down `writer` to
    forward the close to its peer.
- [`src/tests.rs`](src/tests.rs) — in-crate integration tests, compiled only
  under `#[cfg(test)]` (declared as `mod tests;` from `main.rs`). See
  [Testing](#testing).

## How a proxied connection works

`incoming_connection_handle` does the following per accepted client:

1. Wraps the accepted client socket ("source") in a `logged_stream::LoggedStream`
   and `tokio::io::split`s it into read/write halves.
2. Connects a fresh `TcpStream` to `arguments.remote_addr` ("destination"),
   wraps it in another `LoggedStream`, and splits it too.
3. Relays both directions concurrently with `tokio::join!` over two `relay`
   futures (one connection task, not two spawned per-direction tasks), running
   each direction to completion:
   - **destination → source**: copies bytes from the destination to the source.
   - **source → destination**: copies bytes from the source to the destination,
     with each read bounded by a `tokio::time::timeout` of `arguments.timeout`
     seconds (an idle-read timeout on the client side).
   Each `relay` ends at end-of-stream (a `0`-length read), on a read/write error,
   or — for the source side — on an idle timeout, then shuts down its writer to
   forward the close to that peer (a half-close). Because both directions run to
   completion independently (rather than one being cancelled when the other ends),
   data still in flight in the other direction is delivered before the connection
   closes.

### Logging / de-duplication detail (intentional)

Both directions of the conversation are logged on the **source** `LoggedStream`,
which uses `DefaultFilter` (logs read and write payload). A read appears with a
`<` marker, a write with a `>` marker. The **destination** `LoggedStream` uses
`RecordKindFilter` limited to `Drop`, `Error`, and `Shutdown` records — i.e. it
deliberately does **not** re-log payload, because those bytes are the same ones
already shown on the source side. This avoids printing every byte twice. The
console sink is `ConsoleLogger` at the `"debug"` label.

## Dependencies

- `tokio` (with `default-features = false` and only `io-util`, `macros`, `net`,
  `rt-multi-thread`, `time`) — async runtime, TCP, `timeout`, I/O traits.
- `clap` (`std`, `derive`) — CLI parsing.
- `env_logger` + `log` — logging frontend/facade.
- `bytes` — `BytesMut` relay buffers.
- `logged-stream` (`0.6.0`) — the companion crate (same author) that provides
  `LoggedStream`, the `BufferFormatter` implementations
  (`DecimalFormatter`, `LowercaseHexadecimalFormatter`,
  `UppercaseHexadecimalFormatter`, `BinaryFormatter`, `OctalFormatter`),
  `ConsoleLogger`, `DefaultFilter`, `RecordKindFilter`, and `RecordKind`.

## CLI options

```
-l, --level <LEVEL>                          [default: debug]  trace|debug|info|warn|error|off
-b, --bind-listener-addr <SOCKET_ADDR>       address to listen on (IP:port)
-r, --remote-addr <SOCKET_ADDR>              destination address (IP:port)
-t, --timeout <SECONDS>                      source-side read timeout [default: 60]
-f, --formatting <FORMATTING>                [default: lowerhex]  decimal|lowerhex|upperhex|binary|octal
-s, --separator <STRING>                     byte separator in output [default: ":"]
-p, --precision <PRECISION>                  [default: seconds]  seconds|milliseconds|microseconds|nanoseconds
```

Both `--bind-listener-addr` and `--remote-addr` are parsed as `std::net::SocketAddr`,
so they must currently be literal `IP:port` (no hostnames).

## Build / test / lint

These are the same commands CI runs, so they reproduce locally identically:

```sh
cargo build --all-targets                 # build bin + tests
cargo test                                # run the in-crate integration tests
cargo clippy --all-features -- -D warnings
cargo fmt --check                         # rustfmt.toml: imports_granularity="Item", use_field_init_shorthand=true
cargo msrv find                           # verify MSRV (requires cargo-msrv)
```

## Testing

Integration tests live **inside the crate** in [`src/tests.rs`](src/tests.rs)
under `#[cfg(test)]`, rather than in a top-level `tests/` directory. This is a
deliberate choice: a `tests/` directory can only exercise a crate's public
**library** API, which would require adding a `lib` target and publishing a
library surface on docs.rs. Keeping the tests in-crate lets them call internal
(`pub(crate)`) functions directly while the package stays binary-only.

The tests are fully self-contained and portable:

- Each test spins up its own minimal **echo server** written with plain Tokio
  (no external tools like `python`/`netcat`, no extra dev-dependencies).
- Both the echo server and the proxy listener bind to `127.0.0.1:0`, letting the
  OS assign ephemeral ports — so parallel test/CI jobs never collide and there
  are no hardcoded ports. Always use the literal `127.0.0.1` (not `localhost`,
  which can resolve to IPv6 on Windows).
- All network I/O is wrapped in `tokio::time::timeout`, so tests fail fast
  instead of hanging and avoid `sleep`-based flakiness.
- Tests run via `cargo test`, identically on the developer machine and in CI
  (ubuntu/macos/windows × stable/beta/nightly).

## Continuous integration

[`.github/workflows/check.yml`](.github/workflows/check.yml) runs on push to
`master`, on PRs, and via manual dispatch:

- **clippy** — `cargo clippy --all-features -- -D warnings` on stable, beta, nightly.
- **fmt** — `cargo fmt --check`.
- **build_and_test** — `cargo build --all-targets` then `cargo test` on
  ubuntu/macos/windows × stable/beta/nightly.
- **msrv** — `cargo msrv find` to verify the minimal supported Rust version.

Other workflows are housekeeping: `labeler.yml` (PR labels) and
`prs-cache-clean.yml` (cache cleanup). Dependency updates are automated by
Dependabot (`.github/dependabot.yml`).

## Conventions

- Formatting is enforced by `rustfmt` with [`rustfmt.toml`](rustfmt.toml):
  one import per line (`imports_granularity = "Item"`) and field-init shorthand
  (`use_field_init_shorthand = true`). Run `cargo fmt` before committing.
- Non-Rust files are formatted with Prettier (`.prettierrc`).
- Keep the binary-only shape: prefer `pub(crate)` for internals that tests need,
  rather than introducing a public `lib` target.
