use bytes::BytesMut;
use clap::Parser;
use logged_stream::ConsoleLogger;
use logged_stream::DefaultFilter;
use logged_stream::HexDecimalFormatter;
use logged_stream::LoggedStream;
use logged_stream::RecordKind;
use logged_stream::RecordKindFilter;
use std::env;
use std::net;
use std::time::Duration;
use tokio::io::{self, AsyncReadExt, AsyncWriteExt};
use tokio::net as tokio_net;
use tokio::time::timeout;

#[derive(Debug, Clone, Parser)]
#[command(next_line_help = true)]
#[command(author, version, about, long_about = None)]
struct Arguments {
    /// Application logging level.
    #[arg(short, long, default_value = "debug")]
    level: String,
    /// Address on which TCP listener should be binded.
    #[arg(short, long)]
    bind_listener_addr: net::SocketAddr,
    /// Address of remote server.
    #[arg(short, long)]
    remote_addr: net::SocketAddr,
    /// Incoming connection reading timeout.
    #[arg(short, long, default_value = "60")]
    timeout: u64,
}

async fn incoming_connection_handle(arguments: Arguments, source_stream: tokio_net::TcpStream) {
    let (mut source_stream_read_half, mut source_stream_write_half) = io::split(LoggedStream::new(
        source_stream,
        HexDecimalFormatter::new(None),
        DefaultFilter::default(),
        ConsoleLogger::new_unchecked("debug"),
    ));
    let destination_stream = tokio_net::TcpStream::connect(arguments.remote_addr)
        .await
        .expect("Failed to connect to destination address");
    let (mut destination_stream_read_half, mut destination_stream_write_half) =
        io::split(LoggedStream::new(
            destination_stream,
            HexDecimalFormatter::new(None),
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

    env::set_var("RUST_LOG", arguments.level.clone());
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
