# logged-tcp-proxy

## Table of contents

-   [Description](#description)
-   [Usage](#usage)
-   [Options](#options)
-   [Example](#example)
-   [License](#license)
-   [Contribution](#contribution)

## Description

This repository provides a command line utility that allows you to proxy TCP connections with payload output to the console.

## Usage

To use `logged-tcp-proxy`, at first clone project using the following command:

```sh
$ git clone git@github.com:qwerty541/logged-tcp-proxy.git
```

Than you need to compile crate by running the following command in project directory:

```sh
$ cargo build
```

Now you can run compiled binary:

```sh
$ ./target/debug/logged_tcp_proxy -l debug -b 127.0.0.1:20502 -r 127.0.0.1:20582
```

## Options

Below is a list of currently supported options.

```
$ ./target/debug/logged_tcp_proxy --help

Usage: logged_tcp_proxy [OPTIONS] --bind-listener-addr <BIND_LISTENER_ADDR> --remote-addr <REMOTE_ADDR>

Options:
  -l, --level <LEVEL>
          Application logging level [default: debug]
  -b, --bind-listener-addr <BIND_LISTENER_ADDR>
          Address on which TCP listener should be binded
  -r, --remote-addr <REMOTE_ADDR>
          Address of remote server
  -t, --timeout <TIMEOUT>
          Incoming connection reading timeout [default: 60]
  -h, --help
          Print help
  -V, --version
          Print version
```

## Example

Below is an example of using this command line tool as proxy between device and data storage server with command and console output.

```
$ ./target/debug/logged_tcp_proxy -l debug -b 127.0.0.1:20502 -r 127.0.0.1:20582
[2023-04-19T13:48:02Z INFO  logged_tcp_proxy] Listener binded, waiting for incoming connections...
[2023-04-19T13:48:24Z INFO  logged_tcp_proxy] Incoming connection from 127.0.0.1:46306
[2023-04-19T13:48:24Z DEBUG logged_stream::logger] < 00:00:00:00:00:19:6f:03:16:00:1f:00:20:00:11:00:22:00:33:00:44:00:55:00:66:00:01:00:00:00:00
[2023-04-19T13:48:24Z DEBUG logged_stream::logger] > 00:00:00:00:00:0b:6f:10:03:f1:00:02:04:00:00:00:00
[2023-04-19T13:48:24Z DEBUG logged_stream::logger] < 00:00:00:00:00:06:6f:10:03:f1:00:02
[2023-04-19T13:48:25Z DEBUG logged_stream::logger] > 00:01:00:00:00:06:6f:03:00:7a:00:01:00:02:00:00:00:06:01:01:00:01:00:01:00:03:00:00:00:06:01:02:00:01:00:01:00:04:00:00:00:06:01:03:00:01:00:10:00:05:00:00:00:06:01:03:00:11:00:01:00:06:00:00:00:06:01:03:00:7b:00:01:00:07:00:00:00:06:01:03:0f:a0:00:01:00:08:00:00:00:06:01:03:13:88:00:03
[2023-04-19T13:48:25Z DEBUG logged_stream::logger] < 00:01:00:00:00:05:6f:03:02:02:ff:00:02:00:00:00:04:01:01:01:01:00:03:00:00:00:04:01:02:01:01:00:04:00:00:00:23:01:03:20:00:7b:00:0c:ff:ff:ff:ff:ff:ff:ff:ff:ff:ff:ff:ff:01:36:40:49:0f:db:40:09:21:fb:54:44:2d:18:ff:ff:00:05:00:00:00:05:01:03:02:ff:ff:00:06:00:00:00:05:01:03:02:00:01:00:07:00:00:00:03:01:83:02:00:08:00:00:00:09:01:03:06:00:01:00:02:00:03
[2023-04-19T13:48:25Z DEBUG logged_stream::logger] > 00:09:00:00:00:06:6f:03:00:7b:00:02
[2023-04-19T13:48:25Z DEBUG logged_stream::logger] > 00:0a:00:00:00:06:01:04:00:01:00:01
[2023-04-19T13:48:25Z DEBUG logged_stream::logger] < 00:09:00:00:00:07:6f:03:04:00:00:00:01:00:0a:00:00:00:05:01:04:02:00:7b
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
