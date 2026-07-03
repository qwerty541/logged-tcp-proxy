# logged-tcp-proxy <!-- omit in toc -->

[![Crates.io version][crates-version-badge]][crates-url]
[![Crates.io downloads][crates-downloads-badge]][crates-url]
[![Rust version][rust-version]][rust-url]
[![License][license-badge]][license-url]
[![Workflow Status][workflow-badge]][actions-url]
[![Lines count][sloc-badge]][scc-repo-url]
[![Cocomo][cocomo-badge]][scc-repo-url]

[crates-version-badge]: https://img.shields.io/crates/v/logged_tcp_proxy.svg
[crates-downloads-badge]: https://img.shields.io/crates/d/logged_tcp_proxy.svg
[crates-url]: https://crates.io/crates/logged_tcp_proxy
[license-badge]: https://img.shields.io/crates/l/logged_tcp_proxy.svg
[license-url]: https://github.com/qwerty541/logged-tcp-proxy/blob/master/LICENSE-MIT
[workflow-badge]: https://github.com/qwerty541/logged-tcp-proxy/workflows/check/badge.svg
[actions-url]: https://github.com/qwerty541/logged-tcp-proxy/actions
[rust-version]: https://img.shields.io/badge/rust-1.85.1%2B-lightgrey.svg?logo=rust
[rust-url]: https://blog.rust-lang.org/
[sloc-badge]: https://sloc.xyz/github/qwerty541/logged-tcp-proxy/?badge-bg-color=2081C2
[cocomo-badge]: https://sloc.xyz/github/qwerty541/logged-tcp-proxy/?badge-bg-color=2081C2&category=cocomo
[scc-repo-url]: https://github.com/boyter/scc

<details>
<summary>Table of contents</summary>

- [Description](#description)
- [Features](#features)
- [Installation](#installation)
  - [From crates.io (Recommended)](#from-cratesio-recommended)
  - [From git repository](#from-git-repository)
- [Quickstart](#quickstart)
- [Options](#options)
- [Example](#example)
- [License](#license)
- [Contribution](#contribution)
</details>

## Description

`logged_tcp_proxy` is a small command-line TCP proxy for debugging binary protocols.
You place it between a client and a server: it listens on a local address, relays
bytes in both directions unchanged, and prints the raw payload of the conversation to
your console so you can see exactly what is being sent. In other words, it is a
transparent man-in-the-middle — point your client at the proxy's listen address
(`--bind-listener-addr`) instead of the real server, and set the real server as
`--remote-addr`.

It was originally written to debug MODBUS/TCP traffic, but it parses nothing and works
for any TCP protocol. Payload output can be formatted as hexadecimal (lowercase or
uppercase), decimal, octal, or binary, with a configurable byte separator.

## Features

- Transparent bidirectional TCP relay (man-in-the-middle) — parses nothing, forwards
  bytes unchanged, and preserves half-close so data still in flight in the other
  direction is delivered before the connection closes.
- Logs the payload in lowercase hex, uppercase hex, decimal, octal, or binary, with a
  configurable byte separator (`--separator`).
- Optional whole-connection idle timeout (`--timeout`); waits indefinitely by default.
- Bounded concurrency with backpressure (`--max-connections`, default 512) — serves
  many clients at once and stops accepting new ones only when at capacity.
- Configurable async runtime worker threads (`--threads`, default 4).
- Configurable timestamp precision (`--precision`) and logging level (`--level`).
- Graceful shutdown on Ctrl-C (exits with status 0).

## Installation

### From crates.io (Recommended)

Run the following command and wait until the crate is compiled:

```sh
$ cargo install logged_tcp_proxy
```

Now you can run compiled binary:

```sh
$ logged_tcp_proxy --bind-listener-addr 127.0.0.1:20502 --remote-addr 127.0.0.1:20582
```

### From git repository

Run the following command and wait until the crate is compiled:

```sh
$ cargo install --git https://github.com/qwerty541/logged-tcp-proxy.git --tag v0.2.0 logged_tcp_proxy
```

Also you can remove tag option to install the latest development version.

Now you can run compiled binary:

```sh
$ logged_tcp_proxy --bind-listener-addr 127.0.0.1:20502 --remote-addr 127.0.0.1:20582
```

## Quickstart

To see traffic flowing you need three processes: a destination server, the proxy, and
a client. The walkthrough below uses only Python's standard library and `curl`, so
nothing extra needs to be installed.

**1. Start a throwaway destination server** on port `20582` (this stands in for the
real server whose traffic you want to inspect):

```sh
python3 -m http.server 20582
```

**2. Run the proxy** in a second terminal, listening on `20502` and forwarding to the
server from step 1:

```sh
logged_tcp_proxy --bind-listener-addr 127.0.0.1:20502 --remote-addr 127.0.0.1:20582
```

**3. Send a request through the proxy** — to port `20502` (the proxy), not `20582`
(the server) — from a third terminal:

```sh
curl http://127.0.0.1:20502/
```

The request and response bytes now appear in the proxy's console, each line marked
`<` (bytes read from the client) or `>` (bytes written back to it). Press Ctrl-C to
stop the proxy; it shuts down cleanly and exits with status `0`. See the
[Example](#example) below for an annotated run and how to read the output.

> **Note:** `--remote-addr` must point at an address where something is already
> listening. If nothing is there, the proxy logs `Failed to connect to destination ...`,
> closes that client, and keeps serving other connections — no payload is printed.

## Options

Below is a list of currently supported options.

```
$ logged_tcp_proxy --help
Command line interface for proxying TCP connections with payload output into console which can be formatted different ways.

Usage: logged_tcp_proxy [OPTIONS] --bind-listener-addr <BIND_LISTENER_ADDR> --remote-addr <REMOTE_ADDR>

Options:
  -l, --level <LEVEL>
          Application logging level [default: debug] [possible values: trace, debug, info, warn, error, off]
  -b, --bind-listener-addr <BIND_LISTENER_ADDR>
          Address on which TCP listener should be binded
  -r, --remote-addr <REMOTE_ADDR>
          Address of remote server
  -t, --timeout <TIMEOUT>
          Idle timeout for the connection, in seconds: the connection is closed once both directions have been silent for this long. If omitted, the proxy waits indefinitely (until a peer closes the connection or Ctrl-C)
  -m, --max-connections <MAX_CONNECTIONS>
          Maximum number of connections processed concurrently. Once this many are active, further incoming connections wait until a slot frees [default: 512]
  -w, --threads <THREADS>
          Number of worker threads used by the async runtime. Raise it to handle more concurrent traffic on multi-core machines [default: 4]
  -f, --formatting <FORMATTING>
          Formatting of console payload output, [default: lowerhex] [possible values: decimal, lowerhex, upperhex, binary, octal]
  -s, --separator <SEPARATOR>
          Console payload output bytes separator [default: :]
  -p, --precision <PRECISION>
          Timestamp precision [default: seconds] [possible values: seconds, milliseconds, microseconds, nanoseconds]
  -h, --help
          Print help
  -V, --version
          Print version
```

> **Note:** the relayed payload is logged at the `debug` level. Keep `--level` at
> `debug` (the default) or `trace` to see it — setting `--level info` or higher hides
> the payload and leaves only the lifecycle (`INFO`) lines.

## Example

Below is an annotated run proxying a MODBUS/TCP exchange — the command that is run,
followed by the console output it produces.

```
$ logged_tcp_proxy --bind-listener-addr 127.0.0.1:20502 --remote-addr 127.0.0.1:20582
[2023-05-04T02:39:33Z INFO] Listener bound to 127.0.0.1:20502, waiting for incoming connections...
[2023-05-04T02:39:37Z INFO] Incoming connection from 127.0.0.1:50376
[2023-05-04T02:39:37Z DEBUG] < 00:00:00:00:00:19:6f:03:16:00:1f:00:20:00:11:00:22:00:33:00:44:00:55:00:66:00:01:00:00:00:00
[2023-05-04T02:39:37Z DEBUG] > 00:00:00:00:00:0b:6f:10:03:f1:00:02:04:00:00:00:00
[2023-05-04T02:39:37Z DEBUG] < 00:00:00:00:00:06:6f:10:03:f1:00:02
[2023-05-04T02:39:37Z DEBUG] > 00:01:00:00:00:06:6f:03:00:7a:00:01:00:02:00:00:00:06:6f:03:00:7b:00:02
[2023-05-04T02:39:37Z DEBUG] < 00:01:00:00:00:05:6f:03:02:02:ff:00:02:00:00:00:07:6f:03:04:00:00:00:01
[2023-05-04T02:39:37Z DEBUG] > 00:03:00:00:00:06:01:01:00:01:00:01:00:04:00:00:00:06:01:02:00:01:00:01:00:05:00:00:00:06:01:03:00:01:00:10:00:06:00:00:00:06:01:03:00:11:00:01:00:07:00:00:00:06:01:03:00:7b:00:01:00:08:00:00:00:06:01:03:0f:a0:00:01:00:09:00:00:00:06:01:03:13:88:00:03:00:0a:00:00:00:06:01:04:00:01:00:01
[2023-05-04T02:39:37Z DEBUG] < 00:03:00:00:00:04:01:01:01:01:00:04:00:00:00:04:01:02:01:01:00:05:00:00:00:23:01:03:20:00:7b:00:0c:ff:ff:ff:ff:ff:ff:ff:ff:ff:ff:ff:ff:01:36:40:49:0f:db:40:09:21:fb:54:44:2d:18:ff:ff:00:06:00:00:00:05:01:03:02:ff:ff:00:07:00:00:00:05:01:03:02:00:01:00:08:00:00:00:03:01:83:02:00:09:00:00:00:09:01:03:06:00:01:00:02:00:03:00:0a:00:00:00:05:01:04:02:00:7b
[2023-05-04T02:40:18Z DEBUG] > 00:0b:00:00:00:06:6f:03:03:e8:00:01
[2023-05-04T02:40:18Z DEBUG] < 00:0b:00:00:00:05:6f:03:02:00:00
```

How to read this output:

- `INFO` lines are lifecycle events — the listener binding (`Listener bound to ...`,
  the proxy's "ready" signal) and each accepted connection (`Incoming connection from ...`).
- `DEBUG` lines are the relayed payload, shown here in the default lowercase-hex
  format with `:` separators (change with `--formatting` and `--separator`).
- `<` marks bytes **read from the client** (source) side; `>` marks bytes **written
  back to the client** (these originate from the remote server).
- Both directions of the conversation are logged once, on the client (source)
  connection, so the same bytes are never printed twice.
- The leading `[...Z ...]` is the timestamp, at `--precision` granularity.

## License

Licensed under either of

-   Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or https://www.apache.org/licenses/LICENSE-2.0)
-   MIT license ([LICENSE-MIT](LICENSE-MIT) or https://opensource.org/licenses/MIT)

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
