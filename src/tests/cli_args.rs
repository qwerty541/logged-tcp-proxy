//! Pure command-line parsing: clap's range validation, defaults, value-enum names
//! and the `--remote-addr` grammar plus its error messages. No sockets involved.

use crate::args::Arguments;
use crate::args::LoggingLevel;
use crate::args::PayloadFormattingKind;
use crate::args::TargetAddr;
use crate::args::TimestampPrecision;

/// `--timeout` is range-validated by clap: `0` and values large enough to overflow
/// the monotonic clock are rejected, a normal value parses, and omitting it yields
/// `None` (no timeout).
#[test]
fn timeout_argument_is_range_validated() {
    use clap::Parser;

    fn parse(extra: &[&str]) -> Result<Arguments, clap::Error> {
        let mut argv = vec!["logged_tcp_proxy", "-b", "127.0.0.1:0", "-r", "127.0.0.1:0"];
        argv.extend_from_slice(extra);
        Arguments::try_parse_from(argv)
    }

    assert_eq!(
        parse(&[]).expect("omitting --timeout should parse").timeout,
        None,
    );
    assert_eq!(
        parse(&["-t", "30"])
            .expect("a normal --timeout should parse")
            .timeout,
        Some(30),
    );
    assert!(parse(&["-t", "1"]).is_ok(), "the minimum (1) is accepted");
    assert!(
        parse(&["-t", "3153600000"]).is_ok(),
        "the maximum (~100 years) is accepted"
    );
    assert!(parse(&["-t", "0"]).is_err(), "0 is rejected");
    assert!(
        parse(&["-t", "3153600001"]).is_err(),
        "above the maximum is rejected"
    );
    assert!(
        parse(&["-t", "18446744073709551615"]).is_err(),
        "u64::MAX (which would overflow the clock) is rejected"
    );
}

/// `--max-connections` has a default and rejects 0 (which would accept nothing).
#[test]
fn max_connections_has_a_default_and_rejects_zero() {
    use clap::Parser;

    fn parse(extra: &[&str]) -> Result<Arguments, clap::Error> {
        let mut argv = vec!["logged_tcp_proxy", "-b", "127.0.0.1:0", "-r", "127.0.0.1:0"];
        argv.extend_from_slice(extra);
        Arguments::try_parse_from(argv)
    }

    assert_eq!(
        parse(&[])
            .expect("default max-connections should parse")
            .max_connections,
        512,
    );
    assert_eq!(
        parse(&["-m", "32"])
            .expect("an explicit max-connections should parse")
            .max_connections,
        32,
    );
    assert!(parse(&["-m", "0"]).is_err(), "0 is rejected");
}

/// Per-connection id tags are on by default and `--no-connection-ids` is the
/// opt-out: the flag takes no value and flips the positively-named
/// `connection_ids` field to `false`.
#[test]
fn connection_ids_default_on_with_no_connection_ids_opt_out() {
    use clap::Parser;

    fn parse(extra: &[&str]) -> Result<Arguments, clap::Error> {
        let mut argv = vec!["logged_tcp_proxy", "-b", "127.0.0.1:0", "-r", "127.0.0.1:0"];
        argv.extend_from_slice(extra);
        Arguments::try_parse_from(argv)
    }

    assert!(
        parse(&[])
            .expect("omitting --no-connection-ids should parse")
            .connection_ids,
        "connection ids must be enabled by default"
    );
    assert!(
        !parse(&["--no-connection-ids"])
            .expect("--no-connection-ids should parse")
            .connection_ids,
        "--no-connection-ids must disable connection ids"
    );
    assert!(
        !parse(&["-n"])
            .expect("the -n short flag should parse")
            .connection_ids,
        "-n is the short alias for --no-connection-ids",
    );
    assert!(
        parse(&["--no-connection-ids", "true"]).is_err(),
        "the flag takes no value"
    );
}

/// Every option's short flag, pinned to the letter it has always had.
///
/// The letters are written out in [`Arguments`] rather than derived, because a bare
/// `#[arg(short)]` takes its letter from the *field* name: renaming a field would
/// silently move a public flag (the long name can be pinned separately, so `--help`
/// need not even look different), or collide with another option. clap enforces
/// uniqueness only through a debug assertion, so a collision panics under
/// `cargo test` but a release build — which is what `cargo install` produces —
/// happily ships two options sharing a letter. This test is what fails if a letter
/// is changed, dropped or duplicated.
#[test]
fn short_flags_are_pinned_and_unique() {
    use clap::CommandFactory;

    let command = Arguments::command();
    let mut actual: Vec<(char, String)> = command
        .get_arguments()
        .filter_map(|argument| Some((argument.get_short()?, argument.get_long()?.to_string())))
        .collect();
    actual.sort();

    // Only the options this crate declares: clap injects its own `-h` / `-V` later,
    // when the `Command` is built, so they are deliberately not listed here.
    let expected: Vec<(char, String)> = [
        ('b', "bind-listener-addr"),
        ('f', "formatting"),
        ('l', "level"),
        ('m', "max-connections"),
        ('n', "no-connection-ids"),
        ('p', "precision"),
        ('r', "remote-addr"),
        ('s', "separator"),
        ('t', "timeout"),
        ('w', "threads"),
    ]
    .into_iter()
    .map(|(short, long)| (short, long.to_string()))
    .collect();

    assert_eq!(
        actual, expected,
        "the short flag of every option is part of the public CLI: update this list \
         only when the change is intended and documented"
    );

    // Guard the letters directly too, so a duplicate cannot be waved through by
    // being added to `expected` as well.
    let mut letters: Vec<char> = actual.iter().map(|(short, _)| *short).collect();
    let total = letters.len();
    letters.dedup();
    assert_eq!(
        letters.len(),
        total,
        "short flags must be unique; clap only catches duplicates in debug builds"
    );
}

/// The CLI value enums must expose exactly the value names the proxy has always
/// accepted. These names used to come from hand-written `ValueEnum`/`FromStr`/
/// `Display` impls and are now derived (`#[derive(ValueEnum)]` plus the
/// `argument_impl_*` macros). This test pins the derived `to_possible_value()`
/// names — and the `FromStr` → `Display` round-trip — to those exact strings, so a
/// casing change in the derive (e.g. clap's default kebab-case turning `LowerHex`
/// into `lower-hex`) can't silently break `--formatting lowerhex` or any other
/// documented value, or the matching `default_value` on `Arguments`.
#[test]
fn value_enum_names_match_documented_cli_values() {
    use clap::ValueEnum;

    macro_rules! check {
        ($ty:ty, $expected:expr) => {{
            let expected: Vec<String> = $expected.iter().map(|s: &&str| s.to_string()).collect();

            // `to_possible_value()` (now derived) must yield exactly the documented
            // names, in declaration order.
            let names: Vec<String> = <$ty>::value_variants()
                .iter()
                .map(|variant| variant.to_possible_value().unwrap().get_name().to_owned())
                .collect();
            assert_eq!(
                names, expected,
                concat!(
                    stringify!($ty),
                    ": possible-value names drifted from the documented CLI values"
                ),
            );

            // Each documented name must parse back (FromStr) and `Display` must
            // reproduce it unchanged.
            for name in &expected {
                let parsed = name.parse::<$ty>().expect(concat!(
                    stringify!($ty),
                    ": every documented value must parse via FromStr"
                ));
                assert_eq!(
                    parsed.to_string(),
                    *name,
                    concat!(stringify!($ty), ": Display must round-trip the value name"),
                );
            }
        }};
    }

    check!(
        LoggingLevel,
        &["trace", "debug", "info", "warn", "error", "off"]
    );
    check!(
        PayloadFormattingKind,
        &["decimal", "lowerhex", "upperhex", "binary", "octal"]
    );
    check!(
        TimestampPrecision,
        &["seconds", "milliseconds", "microseconds", "nanoseconds"]
    );
}

/// `--threads` has a default and is range-validated: `0` (which Tokio forbids) and
/// values above the cap are rejected, while the bounds and a normal value parse.
#[test]
fn threads_has_a_default_and_is_range_validated() {
    use clap::Parser;

    fn parse(extra: &[&str]) -> Result<Arguments, clap::Error> {
        let mut argv = vec!["logged_tcp_proxy", "-b", "127.0.0.1:0", "-r", "127.0.0.1:0"];
        argv.extend_from_slice(extra);
        Arguments::try_parse_from(argv)
    }

    assert_eq!(parse(&[]).expect("default threads should parse").threads, 4,);
    assert_eq!(
        parse(&["--threads", "16"])
            .expect("an explicit threads should parse")
            .threads,
        16,
    );
    assert_eq!(
        parse(&["-w", "16"])
            .expect("the -w short flag should parse")
            .threads,
        16,
        "-w is the short alias for --threads",
    );
    assert!(
        parse(&["--threads", "1"]).is_ok(),
        "the minimum (1) is accepted"
    );
    assert!(
        parse(&["--threads", "1024"]).is_ok(),
        "the maximum (1024) is accepted"
    );
    assert!(parse(&["--threads", "0"]).is_err(), "0 is rejected");
    assert!(
        parse(&["--threads", "1025"]).is_err(),
        "above the maximum is rejected"
    );
}

/// `--remote-addr` accepts either a literal `IP:port` (parsed straight to a socket
/// address, never resolved) or a `hostname:port` (kept as a name and resolved
/// lazily at connect time). Malformed values are rejected at parse time without any
/// DNS lookup, so this test needs no network.
#[test]
fn remote_addr_accepts_ip_and_hostname() {
    use clap::Parser;

    fn parse(remote: &str) -> Result<Arguments, clap::Error> {
        Arguments::try_parse_from(["logged_tcp_proxy", "-b", "127.0.0.1:0", "-r", remote])
    }

    // Literal IPv4 and bracketed IPv6 become `Socket` (no DNS involved).
    assert!(matches!(
        parse("127.0.0.1:8080")
            .expect("an IPv4 remote should parse")
            .remote_addr,
        TargetAddr::Socket(_)
    ));
    assert!(matches!(
        parse("[::1]:8080")
            .expect("a bracketed IPv6 remote should parse")
            .remote_addr,
        TargetAddr::Socket(_)
    ));

    // A hostname parses (offline) into `Named`, proving resolution is deferred.
    match parse("example.com:443")
        .expect("a hostname remote should parse")
        .remote_addr
    {
        TargetAddr::Named { host, port } => {
            assert_eq!(host, "example.com");
            assert_eq!(port, 443);
        }
        other => panic!("expected a Named target, got {other:?}"),
    }

    // Malformed remotes are rejected at parse time (no lookup needed).
    assert!(parse("example.com").is_err(), "a missing port is rejected");
    assert!(parse(":443").is_err(), "an empty host is rejected");
    assert!(
        parse("example.com:notaport").is_err(),
        "a non-numeric port is rejected"
    );
    assert!(
        parse("example.com:99999").is_err(),
        "an out-of-range port is rejected"
    );
    assert!(
        parse("2001:db8::1:9000").is_err(),
        "an unbracketed IPv6 literal is rejected (use [address]:port)"
    );

    // Regression guard: a *bracketed* IPv6 with an invalid port must be rejected for
    // the PORT, not misreported as an unbracketed-IPv6 mistake (the input already has
    // brackets). Assert the message via `FromStr` directly, since it is the message
    // that would regress if the host were inspected before the port.
    for bad in ["[::1]:99999", "[::1]:notaport"] {
        let err = bad
            .parse::<TargetAddr>()
            .expect_err("a bracketed IPv6 with a bad port must be rejected");
        assert!(
            err.contains("port"),
            "`{bad}` should report a port error, got: {err}"
        );
        assert!(
            !err.contains("bracketed"),
            "`{bad}` is already bracketed, so it must not report the unbracketed-IPv6 error, got: {err}"
        );
    }
}

/// A rejected `--remote-addr` must be explained by the *actual* problem. Two
/// classes of misleading message are guarded here: a value that already uses the
/// bracketed IPv6 form must never be told to add brackets, and a stray colon that
/// comes from something else (a pasted URL, an extra `:port`) must not be blamed
/// on IPv6 either. A genuinely unbracketed IPv6 literal must still get the
/// bracketing advice, since that is the one case where it is the right fix.
#[test]
fn remote_addr_errors_name_the_actual_problem() {
    fn err(value: &str) -> String {
        value
            .parse::<TargetAddr>()
            .expect_err("value must be rejected")
    }

    // Already bracketed, but the literal itself is malformed (bad hex group, a
    // scope/zone id, or simply not an address): blame the address, not the brackets.
    for value in [
        "[::g]:80",
        "[::1x]:80",
        "[fe80::1%eth0]:80",
        "[not-an-ip]:80",
    ] {
        let message = err(value);
        assert!(
            message.contains("not a valid IPv6 address"),
            "`{value}` should blame the IPv6 literal, got: {message}"
        );
        assert!(
            !message.contains("bracketed"),
            "`{value}` is already bracketed, so it must not advise bracketing, got: {message}"
        );
    }

    // Bracketed, but the port is missing or the bracket is never closed.
    let message = err("[::1]");
    assert!(
        message.contains("the port is missing"),
        "a bracketed address with no port should report the missing port, got: {message}"
    );
    let message = err("[::1:80");
    assert!(
        message.contains("never closed"),
        "an unclosed bracket should be reported as such, got: {message}"
    );

    // Bracketed with a bad port still blames the port (guards the original fix).
    for value in ["[::1]:99999", "[::1]:notaport", "[::1]:80:90"] {
        let message = err(value);
        assert!(
            message.contains("not a valid port number"),
            "`{value}` should blame the port, got: {message}"
        );
        assert!(
            !message.contains("bracketed"),
            "`{value}` must not advise bracketing, got: {message}"
        );
    }

    // A pasted URL is called out as a URL, not as an IPv6 mistake.
    for value in ["http://example.com:443", "tcp://1.2.3.4:80"] {
        let message = err(value);
        assert!(
            message.contains("not a URL"),
            "`{value}` should be reported as a URL, got: {message}"
        );
        assert!(
            !message.contains("bracketed"),
            "`{value}` must not advise IPv6 bracketing, got: {message}"
        );
    }

    // A stray extra colon that is not IPv6 gets a neutral message.
    let message = err("host:80:90");
    assert!(
        !message.contains("bracketed"),
        "`host:80:90` must not advise IPv6 bracketing, got: {message}"
    );

    // A genuinely unbracketed IPv6 literal still gets the bracketing advice.
    for value in ["2001:db8::1:9000", "::1", "fe80::1"] {
        let message = err(value);
        assert!(
            message.contains("bracketed `[address]:port`"),
            "`{value}` should advise the bracketed form, got: {message}"
        );
    }
}
