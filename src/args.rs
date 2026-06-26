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
                    if variant.to_possible_value().unwrap().matches(s, false) {
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

/// Maximum accepted `--timeout`, in seconds (~100 years). Generous enough to cover
/// any realistic idle timeout, yet small enough that the connection-start instant
/// plus the timeout can never overflow the monotonic clock on any platform — which
/// would otherwise panic the connection task. ("No timeout" is the default anyway,
/// reached by omitting the flag, so there is no need for larger finite values.)
const MAX_TIMEOUT_SECONDS: u64 = 60 * 60 * 24 * 365 * 100;

#[derive(Debug, Clone, Parser)]
#[command(next_line_help = true)]
#[command(author, version, about, long_about = None)]
pub struct Arguments {
    /// Application logging level.
    #[arg(short, long, default_value = "debug")]
    pub level: LoggingLevel,
    /// Address on which TCP listener should be binded.
    #[arg(short, long)]
    pub bind_listener_addr: net::SocketAddr,
    /// Address of remote server.
    #[arg(short, long)]
    pub remote_addr: net::SocketAddr,
    /// Idle timeout for the connection, in seconds: the connection is closed once
    /// both directions have been silent for this long. If omitted, the proxy waits
    /// indefinitely (until a peer closes the connection or Ctrl-C).
    #[arg(short, long, value_parser = clap::value_parser!(u64).range(1..=MAX_TIMEOUT_SECONDS))]
    pub timeout: Option<u64>,
    /// Maximum number of connections processed concurrently. Once this many are
    /// active, further incoming connections wait until a slot frees.
    #[arg(short, long, default_value = "512", value_parser = clap::value_parser!(u32).range(1..))]
    pub max_connections: u32,
    /// Formatting of console payload output,
    #[arg(short, long, default_value = "lowerhex")]
    pub formatting: PayloadFormattingKind,
    /// Console payload output bytes separator.
    #[arg(short, long, default_value = ":")]
    pub separator: String,
    /// Timestamp precision.
    #[arg(short, long, default_value = "seconds")]
    pub precision: TimestampPrecision,
}
