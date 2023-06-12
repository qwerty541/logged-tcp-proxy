# logged-tcp-proxy <!-- omit from toc -->

[![Crates.io][crates-badge]][crates-url]
![Rust version][rust-version]
![License][license-badge]
[![Workflow Status][workflow-badge]][actions-url]

[crates-badge]: https://img.shields.io/crates/v/logged_tcp_proxy.svg
[crates-url]: https://crates.io/crates/logged_tcp_proxy
[license-badge]: https://img.shields.io/crates/l/logged_tcp_proxy.svg
[workflow-badge]: https://github.com/qwerty541/logged-tcp-proxy/workflows/check/badge.svg
[actions-url]: https://github.com/qwerty541/logged-tcp-proxy/actions
[rust-version]: https://img.shields.io/badge/rust-1.68.2%2B-lightgrey.svg?logo=rust

## Table of contents <!-- omit from toc -->

- [Description](#description)
- [Installation](#installation)
  - [From crates.io](#from-cratesio)
  - [From git repository](#from-git-repository)
- [Options](#options)
- [Example](#example)
- [License](#license)
- [Contribution](#contribution)

## Description

This repository provides a command line interface for proxying TCP connections with payload output into the console. Payload output can be formatted in different ways: hexadecimal (lowercase and uppercase), decimal, octal and binary.

## Installation

### From crates.io

Run the following command and wait until the crate is compiled:

```sh
$ cargo install logged_tcp_proxy
```

Now you can run compiled binary:

```sh
$ logged_tcp_proxy -b 127.0.0.1:20502 -r 127.0.0.1:20582
```

### From git repository

At first clone project using the following command:

```sh
$ git clone git@github.com:qwerty541/logged-tcp-proxy.git
```

Than you need to compile crate by running the following command in project directory:

```sh
$ cargo build
```

Now you can run compiled binary:

```sh
$ ./target/debug/logged_tcp_proxy -b 127.0.0.1:20502 -r 127.0.0.1:20582
```

## Options

Below is a list of currently supported options.

```
$ logged_tcp_proxy --help

Usage: logged_tcp_proxy [OPTIONS] --bind-listener-addr <BIND_LISTENER_ADDR> --remote-addr <REMOTE_ADDR>

Options:
  -l, --level <LEVEL>
          Application logging level [default: debug] [possible values: trace, debug, info, warn, error, off]
  -b, --bind-listener-addr <BIND_LISTENER_ADDR>
          Address on which TCP listener should be binded
  -r, --remote-addr <REMOTE_ADDR>
          Address of remote server
  -t, --timeout <TIMEOUT>
          Incoming connection reading timeout [default: 60]
  -f, --formatting <FORMATTING>
          Formatting of console payload output, [default: lowerhex] [possible values: decimal, lowerhex, upperhex, binary, octal]
  -h, --help
          Print help
  -V, --version
          Print version
```

## Example

Below is an example of using this command line tool as proxy between device and data storage server with command and console output.

```
$ logged_tcp_proxy -b 127.0.0.1:20502 -r 127.0.0.1:20582
[2023-05-04T02:39:33Z INFO  logged_tcp_proxy::conn] Listener binded, waiting for incoming connections...
[2023-05-04T02:39:37Z INFO  logged_tcp_proxy::conn] Incoming connection from 127.0.0.1:50376
[2023-05-04T02:39:37Z DEBUG logged_stream::logger] < 00:00:00:00:00:19:6f:03:16:00:1f:00:20:00:11:00:22:00:33:00:44:00:55:00:66:00:01:00:00:00:00
[2023-05-04T02:39:37Z DEBUG logged_stream::logger] > 00:00:00:00:00:0b:6f:10:03:f1:00:02:04:00:00:00:00
[2023-05-04T02:39:37Z DEBUG logged_stream::logger] < 00:00:00:00:00:06:6f:10:03:f1:00:02
[2023-05-04T02:39:37Z DEBUG logged_stream::logger] > 00:01:00:00:00:06:6f:03:00:7a:00:01:00:02:00:00:00:06:6f:03:00:7b:00:02
[2023-05-04T02:39:37Z DEBUG logged_stream::logger] < 00:01:00:00:00:05:6f:03:02:02:ff:00:02:00:00:00:07:6f:03:04:00:00:00:01
[2023-05-04T02:39:37Z DEBUG logged_stream::logger] > 00:03:00:00:00:06:01:01:00:01:00:01:00:04:00:00:00:06:01:02:00:01:00:01:00:05:00:00:00:06:01:03:00:01:00:10:00:06:00:00:00:06:01:03:00:11:00:01:00:07:00:00:00:06:01:03:00:7b:00:01:00:08:00:00:00:06:01:03:0f:a0:00:01:00:09:00:00:00:06:01:03:13:88:00:03:00:0a:00:00:00:06:01:04:00:01:00:01
[2023-05-04T02:39:37Z DEBUG logged_stream::logger] < 00:03:00:00:00:04:01:01:01:01:00:04:00:00:00:04:01:02:01:01:00:05:00:00:00:23:01:03:20:00:7b:00:0c:ff:ff:ff:ff:ff:ff:ff:ff:ff:ff:ff:ff:01:36:40:49:0f:db:40:09:21:fb:54:44:2d:18:ff:ff:00:06:00:00:00:05:01:03:02:ff:ff:00:07:00:00:00:05:01:03:02:00:01:00:08:00:00:00:03:01:83:02:00:09:00:00:00:09:01:03:06:00:01:00:02:00:03:00:0a:00:00:00:05:01:04:02:00:7b
[2023-05-04T02:40:18Z DEBUG logged_stream::logger] > 00:0b:00:00:00:06:6f:03:03:e8:00:01
[2023-05-04T02:40:18Z DEBUG logged_stream::logger] < 00:0b:00:00:00:05:6f:03:02:00:00
```

## License

Licensed under either of

-   Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
-   MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
