use crate::args::Arguments;
use crate::args::get_formatter_by_kind;
use bytes::BytesMut;
use logged_stream::ConsoleLogger;
use logged_stream::DefaultFilter;
use logged_stream::LoggedStream;
use logged_stream::RecordKind;
use logged_stream::RecordKindFilter;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::io::{self};
use tokio::net as tokio_net;
use tokio::time::Instant;
use tokio::time::sleep_until;

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

    let bound_addr = listener.local_addr()?;
    log::info!("Listener bound to {bound_addr}, waiting for incoming connections...");

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

    // Relay both directions concurrently, running each to completion. As each
    // direction ends (end-of-stream or a read/write error) it shuts down its
    // writer, forwarding the close to that peer; the other direction keeps relaying
    // until it ends too, so data still in flight is delivered rather than dropped
    // (this correctly handles a peer that half-closes while a response is pending).
    //
    // When `--timeout` is set, a single idle-timeout watchdog runs alongside the
    // relays and tears the connection down once *both* directions have been silent
    // for the timeout. Activity in either direction resets it (via the shared
    // `ActivityClock`), so an actively-transferring one-directional connection is
    // never interrupted.
    match arguments.timeout {
        None => {
            tokio::join!(
                relay(source_stream_read_half, destination_stream_write_half, None),
                relay(destination_stream_read_half, source_stream_write_half, None),
            );
        }
        Some(seconds) => {
            let idle = Duration::from_secs(seconds);
            let clock = ActivityClock::new();
            let relays = async {
                tokio::join!(
                    relay(
                        source_stream_read_half,
                        destination_stream_write_half,
                        Some(&clock),
                    ),
                    relay(
                        destination_stream_read_half,
                        source_stream_write_half,
                        Some(&clock),
                    ),
                );
            };
            tokio::select! {
                _ = relays => {}
                _ = wait_until_idle(&clock, idle) => {
                    log::info!("Closing idle connection after {seconds}s of inactivity");
                }
            }
        }
    }
}

/// Shared "last activity" clock for a connection's idle timeout. It records the
/// most recent moment either direction relayed data, as milliseconds since the
/// connection started; interior mutability lets both relay directions update it
/// through a shared reference.
struct ActivityClock {
    started: Instant,
    last_active_millis: AtomicU64,
}

impl ActivityClock {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            last_active_millis: AtomicU64::new(0),
        }
    }

    /// Record that data just moved in some direction (resets the idle timer).
    fn record(&self) {
        self.last_active_millis
            .store(self.started.elapsed().as_millis() as u64, Ordering::Relaxed);
    }

    /// The instant at which the connection is considered idle for `idle`.
    fn idle_deadline(&self, idle: Duration) -> Instant {
        let last_active = Duration::from_millis(self.last_active_millis.load(Ordering::Relaxed));
        self.started + last_active + idle
    }
}

/// Resolve once the connection has seen no activity in either direction for
/// `idle`, re-arming whenever fresh activity pushes the deadline out.
async fn wait_until_idle(clock: &ActivityClock, idle: Duration) {
    loop {
        sleep_until(clock.idle_deadline(idle)).await;
        if Instant::now() >= clock.idle_deadline(idle) {
            return;
        }
    }
}

/// Copy bytes from `reader` to `writer` until the stream ends or an I/O error
/// occurs, then shut the writer down so the close is forwarded to its peer.
///
/// Each non-empty chunk is recorded on the shared `activity` clock (when one is
/// provided), so the connection's idle-timeout watchdog can tell that this
/// direction is still moving data. The copy ends when `reader` reaches
/// end-of-stream (`read_buf` yields `Ok(0)`) or a read/write fails; treating a
/// zero-length read as end-of-stream (rather than retrying) is what stops a closed
/// peer from being polled in a tight loop. On return the writer is shut down (a
/// half-close); because the opposite direction is driven to completion
/// independently, any data still in flight there is delivered before the
/// connection closes.
async fn relay<R, W>(mut reader: R, mut writer: W, activity: Option<&ActivityClock>)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buffer = BytesMut::with_capacity(2048);
    // The loop ends on a read error (the `while let` binding fails), on end-of-stream
    // (`read_length == 0`), or on a write error.
    while let Ok(read_length) = reader.read_buf(&mut buffer).await {
        if read_length == 0 {
            break;
        }
        if let Some(activity) = activity {
            activity.record();
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
