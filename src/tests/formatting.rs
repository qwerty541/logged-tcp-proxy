//! Payload formatting: `get_formatter_by_kind` must hand back the formatter the
//! user actually asked for, and honor the requested separator — and the payload
//! must be formatted only at the levels that print it.
//!
//! Printing the payload is what this tool exists for, yet every network test runs
//! with the default `lowerhex`. Without these assertions three of the five
//! `--formatting` modes would never be executed at all, so a swapped match arm — or
//! a rendering change in `logged-stream`, which Dependabot bumps automatically —
//! could ship with every check green while `--formatting octal` quietly printed
//! decimal. These are pure unit tests: no sockets, no runtime, microseconds to run.

use crate::args::PayloadFormattingKind;
use crate::args::get_formatter_by_kind;
use crate::conn::client_stream_kinds;
use log::LevelFilter;
use logged_stream::BufferFormatter;
use logged_stream::EventKind;

/// Bytes chosen so that no two formatters render them alike: `0x00` pins the zero
/// padding, `0x01` separates decimal (`1`) from the fixed-width bases (`01`), and
/// `0x6F`/`0xFF` separate the bases from one another and pin hexadecimal letter case.
const SAMPLE: &[u8] = &[0x00, 0x01, 0x6F, 0xFF];

/// Every `--formatting` value renders the payload in its own notation, with the
/// padding and letter case the console output has always used.
#[test]
fn each_formatting_kind_renders_its_own_notation() {
    let cases = [
        (PayloadFormattingKind::Decimal, "0:1:111:255"),
        (PayloadFormattingKind::LowerHex, "00:01:6f:ff"),
        (PayloadFormattingKind::UpperHex, "00:01:6F:FF"),
        (PayloadFormattingKind::Octal, "000:001:157:377"),
        (
            PayloadFormattingKind::Binary,
            "00000000:00000001:01101111:11111111",
        ),
    ];

    for (kind, expected) in cases {
        let rendered = get_formatter_by_kind(kind, ":").format_buffer(SAMPLE);
        assert_eq!(
            rendered, expected,
            "`--formatting {kind}` rendered the payload as `{rendered}`, expected `{expected}`"
        );
    }
}

/// Each kind maps to a *distinct* formatter, so two arms of `get_formatter_by_kind`
/// cannot be swapped or duplicated without a test noticing. (The renderings above
/// are already pairwise different; this states that requirement directly, so the
/// intent survives a future change to `SAMPLE`.)
#[test]
fn no_two_formatting_kinds_render_alike() {
    let kinds = [
        PayloadFormattingKind::Decimal,
        PayloadFormattingKind::LowerHex,
        PayloadFormattingKind::UpperHex,
        PayloadFormattingKind::Octal,
        PayloadFormattingKind::Binary,
    ];

    for (index, kind) in kinds.iter().enumerate() {
        for other in &kinds[index + 1..] {
            assert_ne!(
                get_formatter_by_kind(*kind, ":").format_buffer(SAMPLE),
                get_formatter_by_kind(*other, ":").format_buffer(SAMPLE),
                "`--formatting {kind}` and `--formatting {other}` render identically"
            );
        }
    }
}

/// `--separator` is placed between bytes — and only between them — whatever its
/// length, including the empty string (which runs the bytes together).
#[test]
fn the_separator_is_placed_between_bytes() {
    let payload = &[0xDE, 0xAD, 0xBE];

    for (separator, expected) in [
        (":", "de:ad:be"),
        ("", "deadbe"),
        (" ", "de ad be"),
        (", ", "de, ad, be"),
    ] {
        assert_eq!(
            get_formatter_by_kind(PayloadFormattingKind::LowerHex, separator)
                .format_buffer(payload),
            expected,
            "separator {separator:?} should yield `{expected}`"
        );
    }

    // A single byte has nothing to separate, so the separator never appears.
    assert_eq!(
        get_formatter_by_kind(PayloadFormattingKind::LowerHex, "--").format_buffer(&[0xDE]),
        "de",
    );
}

/// The client stream keeps its reads and writes — the payload — at exactly the
/// levels that print it, `debug` and `trace`. Below them it rejects them by kind, so
/// a payload nobody will see is never formatted (at `--level info` that formatting
/// would be most of the proxy's work). Its errors, logged at `error`, are kept at
/// every level. The levels are spelled out rather than derived from the payload's
/// level in `conn`: the README promises the payload at `debug`.
#[test]
fn the_payload_is_formatted_only_at_levels_that_print_it() {
    let with_payload = &[EventKind::Read, EventKind::Write, EventKind::Error][..];
    let errors_only = &[EventKind::Error][..];

    for (max_level, expected) in [
        (LevelFilter::Off, errors_only),
        (LevelFilter::Error, errors_only),
        (LevelFilter::Warn, errors_only),
        (LevelFilter::Info, errors_only),
        (LevelFilter::Debug, with_payload),
        (LevelFilter::Trace, with_payload),
    ] {
        assert_eq!(
            client_stream_kinds(max_level),
            expected,
            "with nothing logged above `{max_level}`, the client stream should keep {expected:?}"
        );
    }
}
