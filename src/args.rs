use clap::Parser;
use clap::ValueEnum;
use clap::builder::PossibleValue;
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

#[derive(Debug, Clone, Copy)]
pub enum LoggingLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Off,
}

impl ValueEnum for LoggingLevel {
    fn value_variants<'a>() -> &'a [Self] {
        &[
            Self::Trace,
            Self::Debug,
            Self::Info,
            Self::Warn,
            Self::Error,
            Self::Off,
        ]
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        Some(match self {
            Self::Trace => PossibleValue::new("trace"),
            Self::Debug => PossibleValue::new("debug"),
            Self::Info => PossibleValue::new("info"),
            Self::Warn => PossibleValue::new("warn"),
            Self::Error => PossibleValue::new("error"),
            Self::Off => PossibleValue::new("off"),
        })
    }
}

impl FromStr for LoggingLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        for variant in Self::value_variants() {
            if variant.to_possible_value().unwrap().matches(s, false) {
                return Ok(*variant);
            }
        }
        Err(format!("Invalid variant: {s}"))
    }
}

impl fmt::Display for LoggingLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.to_possible_value()
            .expect("no values are skipped")
            .get_name()
            .fmt(f)
    }
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

#[derive(Debug, Clone, Copy)]
pub enum PayloadFormatingKind {
    Decimal,
    LowerHex,
    UpperHex,
    Binary,
    Octal,
}

impl ValueEnum for PayloadFormatingKind {
    fn value_variants<'a>() -> &'a [Self] {
        &[
            Self::Decimal,
            Self::LowerHex,
            Self::UpperHex,
            Self::Binary,
            Self::Octal,
        ]
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        Some(match self {
            Self::Decimal => PossibleValue::new("decimal"),
            Self::LowerHex => PossibleValue::new("lowerhex"),
            Self::UpperHex => PossibleValue::new("upperhex"),
            Self::Binary => PossibleValue::new("binary"),
            Self::Octal => PossibleValue::new("octal"),
        })
    }
}

impl FromStr for PayloadFormatingKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        for variant in Self::value_variants() {
            if variant.to_possible_value().unwrap().matches(s, false) {
                return Ok(*variant);
            }
        }
        Err(format!("Invalid variant: {s}"))
    }
}

impl fmt::Display for PayloadFormatingKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.to_possible_value()
            .expect("no values are skipped")
            .get_name()
            .fmt(f)
    }
}

pub fn get_formatter_by_kind(
    kind: PayloadFormatingKind,
    separator: &str,
) -> Box<dyn BufferFormatter> {
    match kind {
        PayloadFormatingKind::Decimal => Box::new(DecimalFormatter::new(Some(separator))),
        PayloadFormatingKind::LowerHex => {
            Box::new(LowercaseHexadecimalFormatter::new(Some(separator)))
        }
        PayloadFormatingKind::UpperHex => {
            Box::new(UppercaseHexadecimalFormatter::new(Some(separator)))
        }
        PayloadFormatingKind::Binary => Box::new(BinaryFormatter::new(Some(separator))),
        PayloadFormatingKind::Octal => Box::new(OctalFormatter::new(Some(separator))),
    }
}

#[derive(Debug, Clone, Copy)]
pub enum TimestampPrecision {
    Seconds,
    Milliseconds,
    Microseconds,
    Nanoseconds,
}

impl ValueEnum for TimestampPrecision {
    fn value_variants<'a>() -> &'a [Self] {
        &[
            Self::Seconds,
            Self::Milliseconds,
            Self::Microseconds,
            Self::Nanoseconds,
        ]
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        Some(match self {
            Self::Seconds => PossibleValue::new("seconds"),
            Self::Milliseconds => PossibleValue::new("milliseconds"),
            Self::Microseconds => PossibleValue::new("microseconds"),
            Self::Nanoseconds => PossibleValue::new("nanoseconds"),
        })
    }
}

impl FromStr for TimestampPrecision {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        for variant in Self::value_variants() {
            if variant.to_possible_value().unwrap().matches(s, false) {
                return Ok(*variant);
            }
        }
        Err(format!("Invalid variant: {s}"))
    }
}

impl fmt::Display for TimestampPrecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.to_possible_value()
            .expect("no values are skipped")
            .get_name()
            .fmt(f)
    }
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
    pub formatting: PayloadFormatingKind,
    /// Console payload output bytes separator.
    #[arg(short, long, default_value = ":")]
    pub separator: String,
    /// Timestamp precision.
    #[arg(short, long, default_value = "seconds")]
    pub precision: TimestampPrecision,
}
