use clap::Parser;
use clap::ValueEnum;
use env_logger::TimestampPrecision as EnvLoggerTimestampPrecision;
use log::LevelFilter;
use logged_stream::BinaryFormatter;
use logged_stream::BufferFormatter;
use logged_stream::DecimalFormatter;
use logged_stream::LowercaseHexadecimalFormatter;
use logged_stream::OctalFormatter;
use logged_stream::UppercaseHexadecimalFormatter;
use std::fmt;
use std::net;
use std::str::FromStr;

macro_rules! argument_impl_from_str {
    ($type:ty) => {
        impl FromStr for $type {
            type Err = String;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                for variant in Self::value_variants() {
                    if variant
                        .to_possible_value()
                        .expect("no values are skipped")
                        .matches(s, false)
                    {
                        return Ok(*variant);
                    }
                }
                Err(format!("Invalid variant: {}", s))
            }
        }
    };
}

macro_rules! argument_impl_display {
    ($type:ty) => {
        impl fmt::Display for $type {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.to_possible_value()
                    .expect("no values are skipped")
                    .get_name()
                    .fmt(f)
            }
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LoggingLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Off,
}

impl From<LoggingLevel> for LevelFilter {
    fn from(level: LoggingLevel) -> Self {
        match level {
            LoggingLevel::Trace => LevelFilter::Trace,
            LoggingLevel::Debug => LevelFilter::Debug,
            LoggingLevel::Info => LevelFilter::Info,
            LoggingLevel::Warn => LevelFilter::Warn,
            LoggingLevel::Error => LevelFilter::Error,
            LoggingLevel::Off => LevelFilter::Off,
        }
    }
}

argument_impl_from_str!(LoggingLevel);
argument_impl_display!(LoggingLevel);

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PayloadFormattingKind {
    Decimal,
    #[value(name = "lowerhex")]
    LowerHex,
    #[value(name = "upperhex")]
    UpperHex,
    Binary,
    Octal,
}

pub fn get_formatter_by_kind(
    kind: PayloadFormattingKind,
    separator: &str,
) -> Box<dyn BufferFormatter> {
    match kind {
        PayloadFormattingKind::Decimal => Box::new(DecimalFormatter::new(Some(separator))),
        PayloadFormattingKind::LowerHex => {
            Box::new(LowercaseHexadecimalFormatter::new(Some(separator)))
        }
        PayloadFormattingKind::UpperHex => {
            Box::new(UppercaseHexadecimalFormatter::new(Some(separator)))
        }
        PayloadFormattingKind::Binary => Box::new(BinaryFormatter::new(Some(separator))),
        PayloadFormattingKind::Octal => Box::new(OctalFormatter::new(Some(separator))),
    }
}

argument_impl_from_str!(PayloadFormattingKind);
argument_impl_display!(PayloadFormattingKind);

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TimestampPrecision {
    Seconds,
    Milliseconds,
    Microseconds,
    Nanoseconds,
}

impl From<TimestampPrecision> for EnvLoggerTimestampPrecision {
    fn from(precision: TimestampPrecision) -> Self {
        match precision {
            TimestampPrecision::Seconds => EnvLoggerTimestampPrecision::Seconds,
            TimestampPrecision::Milliseconds => EnvLoggerTimestampPrecision::Millis,
            TimestampPrecision::Microseconds => EnvLoggerTimestampPrecision::Micros,
            TimestampPrecision::Nanoseconds => EnvLoggerTimestampPrecision::Nanos,
        }
    }
}

argument_impl_from_str!(TimestampPrecision);
argument_impl_display!(TimestampPrecision);

/// A remote destination supplied on the command line: either a literal socket
/// address (`IP:port`, connected to directly) or a `host:port` whose host is
/// resolved via DNS when a connection is opened. Only `--remote-addr` accepts a
/// hostname; `--bind-listener-addr` stays a literal [`net::SocketAddr`], since a
/// listener binds a concrete local interface rather than a name.
#[derive(Debug, Clone)]
pub enum TargetAddr {
    /// A literal `IP:port`. Connected to directly, without touching DNS.
    Socket(net::SocketAddr),
    /// A `host:port` whose host is resolved to one or more addresses each time a
    /// connection is opened (so DNS changes are picked up between connections).
    Named { host: String, port: u16 },
}

impl FromStr for TargetAddr {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Try a literal socket address first. This accepts both `IPv4:port` and
        // the bracketed `[IPv6]:port` form, so the `host:port` split below never
        // has to disambiguate the colons inside an IPv6 literal.
        if let Ok(addr) = s.parse::<net::SocketAddr>() {
            return Ok(TargetAddr::Socket(addr));
        }
        // Otherwise treat it as `host:port`, splitting on the last colon (a
        // hostname never contains one). Only the *shape* is validated here; the
        // DNS lookup is deferred to connect time.
        match s.rsplit_once(':') {
            None => Err(format!(
                "invalid remote address `{s}`: expected `IP:port` or `host:port`"
            )),
            Some(("", _)) => Err(format!("invalid remote address `{s}`: the host is empty")),
            Some((host, _)) if host.contains(':') => Err(format!(
                "invalid remote address `{s}`: an IPv6 address must use the bracketed `[address]:port` form"
            )),
            Some((host, port)) => match port.parse::<u16>() {
                Ok(port) => Ok(TargetAddr::Named {
                    host: host.to_string(),
                    port,
                }),
                Err(_) => Err(format!(
                    "invalid remote address `{s}`: `{port}` is not a valid port number (0-65535)"
                )),
            },
        }
    }
}

impl fmt::Display for TargetAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TargetAddr::Socket(addr) => addr.fmt(f),
            TargetAddr::Named { host, port } => write!(f, "{host}:{port}"),
        }
    }
}

impl From<net::SocketAddr> for TargetAddr {
    fn from(addr: net::SocketAddr) -> Self {
        TargetAddr::Socket(addr)
    }
}

/// clap value parser for [`TargetAddr`]: validates the `IP:port` / `host:port`
/// shape at parse time (without resolving DNS), so an obviously malformed value
/// is rejected at startup rather than on the first connection.
fn parse_remote_addr(s: &str) -> Result<TargetAddr, String> {
    s.parse()
}

/// Maximum accepted `--timeout`, in seconds (~100 years). Generous enough to cover
/// any realistic idle timeout, yet small enough that the connection-start instant
/// plus the timeout can never overflow the monotonic clock on any platform — which
/// would otherwise panic the connection task. ("No timeout" is the default anyway,
/// reached by omitting the flag, so there is no need for larger finite values.)
const MAX_TIMEOUT_SECONDS: u64 = 60 * 60 * 24 * 365 * 100;

/// Upper bound for `--threads`. The async runtime is almost always I/O bound, so a
/// handful of threads already saturate a typical proxy workload; this cap leaves
/// generous headroom for many-core machines while rejecting absurd values that would
/// try to spawn a pathological number of OS threads. (Tokio also forbids a count of
/// `0`, which the `1..` lower bound on the range already excludes.)
///
/// Typed `i64` to match the bound `clap::value_parser!(u32)` expects for its range
/// (it validates `u32` arguments against an `i64` range, just as `--max-connections`
/// relies on its `1..` literal being inferred as `i64`).
const MAX_THREADS: i64 = 1024;

/// Custom help template to include the source code URL and author name.
const HELP_TEMPLATE: &str = "\
{before-help}{name} {version}

{about}

Author: {author}
Source: https://github.com/qwerty541/logged-tcp-proxy

{usage-heading} {usage}

{all-args}{after-help}
";

#[derive(Debug, Clone, Parser)]
#[command(next_line_help = true)]
#[command(
    author = clap::crate_authors!("\n"),
    version,
    about,
    long_about = None,
    help_template = HELP_TEMPLATE
)]
pub struct Arguments {
    /// Application logging level.
    #[arg(short, long, default_value = "debug")]
    pub level: LoggingLevel,
    /// Address on which the TCP listener should be bound.
    #[arg(short, long)]
    pub bind_listener_addr: net::SocketAddr,
    /// Address of remote server, as `IP:port` or `hostname:port` (a hostname is
    /// resolved via DNS when each connection is opened).
    #[arg(short, long, value_parser = parse_remote_addr)]
    pub remote_addr: TargetAddr,
    /// Idle timeout for the connection, in seconds: the connection is closed once
    /// both directions have been silent for this long. If omitted, the proxy waits
    /// indefinitely (until a peer closes the connection or Ctrl-C).
    #[arg(short, long, value_parser = clap::value_parser!(u64).range(1..=MAX_TIMEOUT_SECONDS))]
    pub timeout: Option<u64>,
    /// Maximum number of connections processed concurrently. Once this many are
    /// active, further incoming connections wait until a slot frees.
    #[arg(short, long, default_value = "512", value_parser = clap::value_parser!(u32).range(1..))]
    pub max_connections: u32,
    /// Number of worker threads used by the async runtime. Raise it to handle more
    /// concurrent traffic on multi-core machines.
    // `short = 'w'` ("worker"): the natural `-t` is already taken by `--timeout`.
    #[arg(short = 'w', long, default_value = "4", value_parser = clap::value_parser!(u32).range(1..=MAX_THREADS))]
    pub threads: u32,
    /// Formatting of console payload output.
    #[arg(short, long, default_value = "lowerhex")]
    pub formatting: PayloadFormattingKind,
    /// Console payload output bytes separator.
    #[arg(short, long, default_value = ":")]
    pub separator: String,
    /// Timestamp precision.
    #[arg(short, long, default_value = "seconds")]
    pub precision: TimestampPrecision,
}
