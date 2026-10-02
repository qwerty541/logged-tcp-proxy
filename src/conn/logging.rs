//! The per-connection `[#N] ` console tag: its grammar (the two delimiter
//! constants) and the [`ConnLog`] facade that every line belonging to one proxied
//! connection is logged through.

use crate::args::Arguments;
use std::fmt;

/// Opening delimiter of a connection's `[#N] ` console tag.
///
/// The tag's grammar lives here rather than being spelled out at each site that
/// produces or parses it, so a change to the shape cannot leave a parser quietly
/// matching nothing (see `strip_conn_tag` in `crate::tests::log_capture`).
//
// The reference to `log_capture` above is plain code text, not an intra-doc link:
// `mod tests` is `#[cfg(test)]`-gated (main.rs), so rustdoc — which documents the
// crate without `cfg(test)` — can never resolve a path into it, and a link form
// would be a permanent `broken_intra_doc_links` warning. The `docs` CI job builds
// the docs with `-D warnings`, so such a link fails the build.
pub(crate) const CONN_TAG_OPEN: &str = "[#";
/// Closing delimiter of a connection's `[#N] ` console tag. The trailing space is
/// part of it: [`ConsoleLogger`](logged_stream::ConsoleLogger) renders the prefix
/// verbatim, immediately before the record-kind character, with no separator of
/// its own.
pub(crate) const CONN_TAG_CLOSE: &str = "] ";

/// Everything one proxied connection logs, carrying that connection's id tag.
///
/// The tag is `"[#N] "` (see [`CONN_TAG_OPEN`] / [`CONN_TAG_CLOSE`]), or an empty
/// string when `--no-connection-ids` disabled the tags — an empty prefix renders
/// byte-for-byte like no prefix at all, so the disabled path reproduces the
/// untagged output exactly through the same code.
///
/// Every per-connection line goes through this type: the lifecycle lines via
/// [`trace`](Self::trace) / [`debug`](Self::debug) / [`info`](Self::info) /
/// [`warn`](Self::warn) / [`error`](Self::error), and both `LoggedStream`s' console
/// records via [`prefix`](Self::prefix). That is what keeps the tag from being
/// forgotten — a new per-connection line cannot be logged without one, so the
/// "every line of a connection is attributable" guarantee is structural rather than
/// a convention each future call site has to remember. The `prefix` field is private
/// to this module, so [`new`](Self::new) — and with it the `--no-connection-ids`
/// branch — is the only way to obtain one.
pub(super) struct ConnLog {
    prefix: String,
}

impl ConnLog {
    /// Build the logger for connection `conn_id`, honouring `--no-connection-ids`.
    pub(super) fn new(arguments: &Arguments, conn_id: u64) -> Self {
        Self {
            prefix: if arguments.connection_ids {
                format!("{CONN_TAG_OPEN}{conn_id}{CONN_TAG_CLOSE}")
            } else {
                String::new()
            },
        }
    }

    /// The connection's tag, for
    /// [`ConsoleLogger::with_prefix`](logged_stream::ConsoleLogger::with_prefix).
    pub(super) fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Log a line with the connection's tag at the given level. The `message`
    /// argument is a `format_args!`-style `fmt::Arguments` value, so the caller
    /// can use `{}`-style formatting without allocating a `String`.
    ///
    /// Private to this module: it is the single sink the level methods below route
    /// through, which is what makes "every per-connection line carries the tag" a
    /// choke point rather than a convention.
    fn log(&self, level: log::Level, message: fmt::Arguments<'_>) {
        log::log!(level, "{}{message}", self.prefix);
    }

    /// Log one of the connection's debug lines, tagged, at the `trace` level.
    #[allow(dead_code)]
    pub(super) fn trace(&self, message: fmt::Arguments<'_>) {
        self.log(log::Level::Trace, message);
    }

    /// Log one of the connection's debug lines, tagged, at the `debug` level.
    #[allow(dead_code)]
    pub(super) fn debug(&self, message: fmt::Arguments<'_>) {
        self.log(log::Level::Debug, message);
    }

    /// Log one of the connection's lifecycle lines, tagged, at the `info` level.
    pub(super) fn info(&self, message: fmt::Arguments<'_>) {
        self.log(log::Level::Info, message);
    }

    /// Log one of the connection's warning lines, tagged, at the `warn` level.
    #[allow(dead_code)]
    pub(super) fn warn(&self, message: fmt::Arguments<'_>) {
        self.log(log::Level::Warn, message);
    }

    /// Log one of the connection's failure lines, tagged, at the `error` level.
    pub(super) fn error(&self, message: fmt::Arguments<'_>) {
        self.log(log::Level::Error, message);
    }
}
