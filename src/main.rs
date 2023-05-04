use bytes::BytesMut;
use clap::builder::PossibleValue;
use clap::Parser;
use clap::ValueEnum;
use logged_stream::BinaryFormatter;
use logged_stream::BufferFormatter;
use logged_stream::ConsoleLogger;
use logged_stream::DecimalFormatter;
use logged_stream::DefaultFilter;
use logged_stream::HexDecimalFormatter;
use logged_stream::LoggedStream;
use logged_stream::OctalFormatter;
use logged_stream::RecordKind;
use logged_stream::RecordKindFilter;
use std::env;
use std::fmt;
use std::net;
use std::str::FromStr;
use std::string::ToString;
use std::time::Duration;
use tokio::io::{self, AsyncReadExt, AsyncWriteExt};
use tokio::net as tokio_net;
use tokio::time::timeout;

#[derive(Debug, Clone, Copy)]
enum LoggingLevel {
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
enum PayloadFormatingKind {
    Decimal,
    Hexdecimal,
    Binary,
    Octal,
}

impl ValueEnum for PayloadFormatingKind {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Decimal, Self::Hexdecimal, Self::Binary, Self::Octal]
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        Some(match self {
            Self::Decimal => PossibleValue::new("decimal"),
            Self::Hexdecimal => PossibleValue::new("hexdecimal"),
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

fn get_formatter_by_kind(kind: PayloadFormatingKind) -> Box<dyn BufferFormatter> {
    match kind {
        PayloadFormatingKind::Decimal => Box::new(DecimalFormatter::new(None)),
        PayloadFormatingKind::Hexdecimal => Box::new(HexDecimalFormatter::new(None)),
        PayloadFormatingKind::Binary => Box::new(BinaryFormatter::new(None)),
        PayloadFormatingKind::Octal => Box::new(OctalFormatter::new(None)),
    }
}

#[derive(Debug, Clone, Parser)]
#[command(next_line_help = true)]
#[command(author, version, about, long_about = None)]
struct Arguments {
    /// Application logging level.
    #[arg(short, long, default_value = "debug")]
    level: LoggingLevel,
    /// Address on which TCP listener should be binded.
    #[arg(short, long)]
    bind_listener_addr: net::SocketAddr,
    /// Address of remote server.
    #[arg(short, long)]
    remote_addr: net::SocketAddr,
    /// Incoming connection reading timeout.
    #[arg(short, long, default_value = "60")]
    timeout: u64,
    /// Formatting of console payload output,
    #[arg(short, long, default_value = "hexdecimal")]
    formatting: PayloadFormatingKind,
}

async fn incoming_connection_handle(arguments: Arguments, source_stream: tokio_net::TcpStream) {
    let (mut source_stream_read_half, mut source_stream_write_half) = io::split(LoggedStream::new(
        source_stream,
        get_formatter_by_kind(arguments.formatting),
        DefaultFilter::default(),
        ConsoleLogger::new_unchecked("debug"),
    ));
    let destination_stream = tokio_net::TcpStream::connect(arguments.remote_addr)
        .await
        .expect("Failed to connect to destination address");
    let (mut destination_stream_read_half, mut destination_stream_write_half) =
        io::split(LoggedStream::new(
            destination_stream,
            get_formatter_by_kind(arguments.formatting),
            RecordKindFilter::new(&[RecordKind::Drop, RecordKind::Error, RecordKind::Shutdown]),
            ConsoleLogger::new_unchecked("debug"),
        ));

    // TODO_FUTURE: looks like rustfmt currently does not support let-else syntax, remove skip attribute later
    #[rustfmt::skip]
    let destination_stream_handle = tokio::spawn(async move {
        let mut buffer = BytesMut::with_capacity(2048);
        'destination_stream_handle: loop {
            let Ok(read_length) = destination_stream_read_half.read_buf(&mut buffer).await else { 
                break 'destination_stream_handle;
            };
            if read_length == 0 {
                continue 'destination_stream_handle;
            }
            let Ok(write_length) = source_stream_write_half.write(&buffer[0..read_length]).await else {
                break 'destination_stream_handle;
            };
            assert_eq!(read_length, write_length);
            buffer.clear();
        }
    });

    #[rustfmt::skip]
    tokio::spawn(async move {
        let mut buffer = BytesMut::with_capacity(2048);
        'source_stream_handle: loop {
            let Ok(Ok(read_length)) = timeout(
                Duration::from_secs(arguments.timeout),
                source_stream_read_half.read_buf(&mut buffer)
            ).await else {
                destination_stream_handle.abort();
                break 'source_stream_handle;
            };
            if read_length == 0 {
                continue 'source_stream_handle;
            }
            let Ok(write_length) = destination_stream_write_half.write(&buffer[0..read_length]).await else {
                destination_stream_handle.abort();
                break 'source_stream_handle;    
            };
            assert_eq!(read_length, write_length);
            buffer.clear();
        }
    });
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let arguments = Arguments::parse();

    env::set_var("RUST_LOG", arguments.level.to_string());
    env_logger::init();

    let listener = tokio_net::TcpListener::bind(arguments.bind_listener_addr)
        .await
        .expect("Failed to bind tcp listener");

    log::info!("Listener binded, waiting for incoming connections...");

    loop {
        let cloned_arguments = arguments.clone();
        let accept = listener.accept().await;
        match accept {
            Ok((stream, addr)) => {
                log::info!("Incoming connection from {addr}");
                tokio::spawn(incoming_connection_handle(cloned_arguments, stream));
            }
            Err(e) => {
                log::error!("Failed to accept incoming connection due to {e}");
            }
        }
    }
}
