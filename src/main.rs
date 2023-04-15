use logged_stream::DefaultFilter;
use logged_stream::RecordKind;
use tokio::io::{self, AsyncReadExt, AsyncWriteExt};
use tokio::net as tokio_net;
use tokio::time::timeout;
use std::net;
use std::env;
use std::time::Duration;
use logged_stream::LoggedStream;
use logged_stream::HexDecimalFormatter;
use logged_stream::ConsoleLogger;
use logged_stream::RecordKindFilter;
use bytes::BytesMut;

lazy_static::lazy_static! {
    static ref LISTEN_ADDR: net::SocketAddr = net::SocketAddr::new(
        net::IpAddr::V4(net::Ipv4Addr::new(0, 0, 0, 0)), 20502
    );
    static ref CONNECT_TO_ADDR: net::SocketAddr = net::SocketAddr::new(
        net::IpAddr::V4(net::Ipv4Addr::new(127, 0, 0, 1)), 20582
    );
}

const SOURCE_STREAM_READ_TIMEOUT_SECS: u64 = 60;

async fn incoming_connection_handle(source_stream: tokio_net::TcpStream) {
    let (mut source_stream_read_half, mut source_stream_write_half) = io::split(LoggedStream::new(
        source_stream,
        HexDecimalFormatter::new(None),
        DefaultFilter::default(),
        ConsoleLogger::new_unchecked("debug"),
    ));
    let destination_stream =
        tokio_net::TcpStream::connect(*CONNECT_TO_ADDR).await.expect("Failed to connect to destination address");
    let (mut destination_stream_read_half, mut destination_stream_write_half) = io::split(LoggedStream::new(
        destination_stream,
        HexDecimalFormatter::new(None),
        RecordKindFilter::new(&[RecordKind::Drop, RecordKind::Error, RecordKind::Shutdown]),
        ConsoleLogger::new_unchecked("debug"),
    ));

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

    tokio::spawn(async move {
        let mut buffer = BytesMut::with_capacity(2048);
        'source_stream_handle: loop {
            let Ok(Ok(read_length)) = timeout(Duration::from_secs(SOURCE_STREAM_READ_TIMEOUT_SECS), source_stream_read_half.read_buf(&mut buffer)).await else {
                destination_stream_handle.abort();
                break 'source_stream_handle;
            };
            if read_length == 0 { continue 'source_stream_handle; }
            let Ok(write_length) = destination_stream_write_half.write(&buffer[0..read_length]).await else {
                break 'source_stream_handle;    
            };
            assert_eq!(read_length, write_length);
            buffer.clear();
        }
    });
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    env::set_var("RUST_LOG", "debug");
    env_logger::init();

    let listener = tokio_net::TcpListener::bind(*LISTEN_ADDR).await.expect("Failed to bind tcp listener");

    log::debug!("Listener binded, waiting for incoming connections...");

    loop {
        let accept = listener.accept().await;
        match accept {
            Ok((stream, addr)) => {
                log::debug!("Incoming connection from {addr}");
                tokio::spawn(incoming_connection_handle(stream));
            }
            Err(e) => {
                log::error!("Failed to accept incoming connection due to {e}");
            }
        }
    }
}
