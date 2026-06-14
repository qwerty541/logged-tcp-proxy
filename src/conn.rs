use crate::args::Arguments;
use crate::args::get_formatter_by_kind;
use bytes::BytesMut;
use logged_stream::ConsoleLogger;
use logged_stream::DefaultFilter;
use logged_stream::LoggedStream;
use logged_stream::RecordKind;
use logged_stream::RecordKindFilter;
use std::time::Duration;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::io::{self};
use tokio::net as tokio_net;
use tokio::time::timeout;

pub async fn initialize_tcp_listener(arguments: Arguments) -> io::Result<()> {
    let listener = match tokio_net::TcpListener::bind(arguments.bind_listener_addr).await {
        Ok(listener) => listener,
        Err(error) => {
            log::error!(
                "Failed to bind listener on {}: {error}",
                arguments.bind_listener_addr
            );
            return Err(error);
        }
    };

    log::info!(
        "Listener bound to {}, waiting for incoming connections...",
        arguments.bind_listener_addr
    );

    // Serve until interrupted. `run_accept_loop` never returns on its own, so the
    // `select!` runs the accept loop until Ctrl-C (SIGINT) fires, then stops
    // accepting. Dropping the accept-loop future closes the listener and releases
    // the port; in-flight connections are torn down when the runtime shuts down.
    tokio::select! {
        _ = run_accept_loop(listener, arguments) => {}
        result = tokio::signal::ctrl_c() => match result {
            Ok(()) => log::info!("Received shutdown signal, stopping listener."),
            Err(error) => log::error!("Failed to listen for shutdown signal: {error}"),
        },
    }

    Ok(())
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
    let (source_stream_read_half, source_stream_write_half) = io::split(LoggedStream::new(
        source_stream,
        get_formatter_by_kind(arguments.formatting, arguments.separator.as_str()),
        DefaultFilter,
        ConsoleLogger::new_unchecked("debug"),
    ));
    let destination_stream = match tokio_net::TcpStream::connect(arguments.remote_addr).await {
        Ok(stream) => stream,
        Err(error) => {
            log::error!(
                "Failed to connect to destination {}: {error}",
                arguments.remote_addr
            );
            // Returning drops the source halves, closing the client connection.
            return;
        }
    };
    let (destination_stream_read_half, destination_stream_write_half) =
        io::split(LoggedStream::new(
            destination_stream,
            get_formatter_by_kind(arguments.formatting, arguments.separator.as_str()),
            RecordKindFilter::new(&[RecordKind::Drop, RecordKind::Error, RecordKind::Shutdown]),
            ConsoleLogger::new_unchecked("debug"),
        ));

    // Relay both directions concurrently, running each to completion. The
    // source -> destination direction is bounded by the configured idle-read
    // timeout; the destination -> source direction is not. As each direction ends
    // (end-of-stream, a read/write error, or — for the source side — an idle
    // timeout) it shuts down its writer, forwarding the close to that peer; the
    // other direction keeps relaying until it ends too. Running both to completion
    // (rather than cancelling the second when the first ends) means data still in
    // flight in the other direction is delivered instead of dropped — this
    // correctly handles a peer that half-closes while a response is still pending.
    tokio::join!(
        relay(
            source_stream_read_half,
            destination_stream_write_half,
            Some(Duration::from_secs(arguments.timeout)),
        ),
        relay(destination_stream_read_half, source_stream_write_half, None),
    );
}

/// Copy bytes from `reader` to `writer` until the stream ends or an I/O error
/// occurs, then shut the writer down so the close is forwarded to its peer.
///
/// The copy ends when `reader` reaches end-of-stream (`read_buf` yields `Ok(0)`),
/// when a read or write fails, or — when `read_timeout` is set — when no data
/// arrives within that timeout. Treating a zero-length read as end-of-stream
/// (rather than retrying) is what stops a closed peer from being polled in a tight
/// loop. On return the writer is shut down (a half-close); because the opposite
/// direction is driven to completion independently, any data still in flight there
/// is delivered before the connection closes.
async fn relay<R, W>(mut reader: R, mut writer: W, read_timeout: Option<Duration>)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buffer = BytesMut::with_capacity(2048);
    loop {
        let read_result = match read_timeout {
            Some(duration) => match timeout(duration, reader.read_buf(&mut buffer)).await {
                Ok(read_result) => read_result,
                Err(_elapsed) => break,
            },
            None => reader.read_buf(&mut buffer).await,
        };
        let Ok(read_length) = read_result else {
            break;
        };
        if read_length == 0 {
            break;
        }
        if writer.write_all(&buffer[0..read_length]).await.is_err() {
            break;
        }
        buffer.clear();
    }
    // Forward the end-of-stream to the peer (half-close). Errors are ignored: the
    // writer may already be closed by a failed write or by the peer.
    let _ = writer.shutdown().await;
}
