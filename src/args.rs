use clap::builder::PossibleValue;
use clap::Parser;
use clap::ValueEnum;
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
        Err(format!("Invalid variant: {}", s))
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
        Err(format!("Invalid variant: {}", s))
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
    /// Incoming connection reading timeout.
    #[arg(short, long, default_value = "60")]
    pub timeout: u64,
    /// Formatting of console payload output,
    #[arg(short, long, default_value = "lowerhex")]
    pub formatting: PayloadFormatingKind,
    /// Console payload output bytes separator.
    #[arg(short, long, default_value = ":")]
    pub separator: String,
}
