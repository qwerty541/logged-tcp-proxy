use crate::args::Arguments;
use crate::args::get_formatter_by_kind;
use bytes::BytesMut;
use logged_stream::ConsoleLogger;
use logged_stream::DefaultFilter;
use logged_stream::LoggedStream;
use logged_stream::RecordKind;
use logged_stream::RecordKindFilter;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::{self};
use tokio::net as tokio_net;
use tokio::time::timeout;

pub async fn initialize_tcp_listener(arguments: Arguments) {
    let listener = tokio_net::TcpListener::bind(arguments.bind_listener_addr)
        .await
        .expect("Failed to bind tcp listener");

    log::info!("Listener binded, waiting for incoming connections...");

    run_accept_loop(listener, arguments).await;
}

/// Accept connections on an already-bound listener and spawn a relay handler for
/// each one. Split out from [`initialize_tcp_listener`] so tests can drive it
/// with a listener bound to an ephemeral port.
pub(crate) async fn run_accept_loop(listener: tokio_net::TcpListener, arguments: Arguments) {
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

async fn incoming_connection_handle(arguments: Arguments, source_stream: tokio_net::TcpStream) {
    let (mut source_stream_read_half, mut source_stream_write_half) = io::split(LoggedStream::new(
        source_stream,
        get_formatter_by_kind(arguments.formatting, arguments.separator.as_str()),
        DefaultFilter,
        ConsoleLogger::new_unchecked("debug"),
    ));
    let destination_stream = tokio_net::TcpStream::connect(arguments.remote_addr)
        .await
        .expect("Failed to connect to destination address");
    let (mut destination_stream_read_half, mut destination_stream_write_half) =
        io::split(LoggedStream::new(
            destination_stream,
            get_formatter_by_kind(arguments.formatting, arguments.separator.as_str()),
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
            let write_result = source_stream_write_half
                .write(&buffer[0..read_length])
                .await;
            let Ok(write_length) = write_result else {
                break 'destination_stream_handle;
            };
            assert_eq!(read_length, write_length);
            buffer.clear();
        }
    });

    tokio::spawn(async move {
        let mut buffer = BytesMut::with_capacity(2048);
        'source_stream_handle: loop {
            let read_result = timeout(
                Duration::from_secs(arguments.timeout),
                source_stream_read_half.read_buf(&mut buffer),
            )
            .await;
            let Ok(Ok(read_length)) = read_result else {
                destination_stream_handle.abort();
                break 'source_stream_handle;
            };
            if read_length == 0 {
                continue 'source_stream_handle;
            }
            let write_result = destination_stream_write_half
                .write(&buffer[0..read_length])
                .await;
            let Ok(write_length) = write_result else {
                destination_stream_handle.abort();
                break 'source_stream_handle;
            };
            assert_eq!(read_length, write_length);
            buffer.clear();
        }
    });
}
