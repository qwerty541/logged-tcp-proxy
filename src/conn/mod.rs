//! The networking core: bind and serve, accept connections, dial the destination,
//! and pump bytes between the two in both directions.
//!
//! Three per-connection concerns live in submodules of this one, so the file reads as
//! an uninterrupted connection lifecycle:
//!
//! - [`logging`] — the `[#N] ` console tag and the `ConnLog` facade every
//!   per-connection line is logged through.
//! - [`stats`] — the `ConnStats` record behind each connection's accept line and
//!   close summary, and the byte counters it keeps.
//! - [`idle`] — the activity clock and watchdog behind `--timeout`.

mod idle;
pub(crate) mod logging;
pub(crate) mod stats;

use self::idle::ActivityClock;
use self::idle::wait_until_idle;
use self::stats::ConnStats;
use self::stats::Direction;
use self::stats::RelayEnd;
use self::stats::RelayStats;
use self::stats::Teardown;
use crate::args::Arguments;
use crate::args::TargetAddr;
use crate::args::get_formatter_by_kind;
use bytes::BytesMut;
use logged_stream::ConsoleLogger;
use logged_stream::DefaultFilter;
use logged_stream::LoggedStream;
use logged_stream::RecordKind;
use logged_stream::RecordKindFilter;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::io::{self};
use tokio::net as tokio_net;
use tokio::sync::Semaphore;
use tokio::time::sleep;

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
    // the port; in-flight connections are torn down when the runtime shuts down,
    // each logging its `(interrupted)` close summary as its task is dropped.
    tokio::select! {
        _ = run_accept_loop(listener, arguments) => {}
        result = tokio::signal::ctrl_c() => match result {
            Ok(()) => log::info!("Received shutdown signal, stopping listener."),
            Err(error) => log::error!("Failed to listen for shutdown signal: {error}"),
        },
    }

    Ok(())
}

/// Minimum delay before retrying after a failed `accept()`. Applied to every
/// accept error so the loop can never busy-spin.
pub(crate) const ACCEPT_BACKOFF_MIN: Duration = Duration::from_millis(10);
/// Maximum accept-retry delay. The backoff grows while errors persist (e.g.
/// file-descriptor exhaustion) but never exceeds this.
pub(crate) const ACCEPT_BACKOFF_MAX: Duration = Duration::from_secs(1);

/// The next accept-retry delay: double the current one, capped at
/// [`ACCEPT_BACKOFF_MAX`].
pub(crate) fn next_accept_backoff(current: Duration) -> Duration {
    (current * 2).min(ACCEPT_BACKOFF_MAX)
}

/// Accept connections on an already-bound listener and spawn a relay handler for
/// each one. Split out from [`initialize_tcp_listener`] so tests can drive it
/// with a listener bound to an ephemeral port.
pub(crate) async fn run_accept_loop(listener: tokio_net::TcpListener, arguments: Arguments) {
    // Bound how many connections are handled concurrently. A permit is acquired
    // *before* accepting, so once `--max-connections` are active the loop stops
    // pulling connections off the backlog (natural backpressure) instead of
    // spawning unbounded handlers; each handler holds its permit until it closes.
    let connection_limit = Arc::new(Semaphore::new(arguments.max_connections as usize));
    let mut accept_backoff = ACCEPT_BACKOFF_MIN;
    // Per-connection ids are minted here, sequentially in accept order, starting at
    // 1 for each proxy run. A plain (non-atomic) counter is deliberate: this accept
    // loop is a single task and the only writer, and each spawned handler receives
    // the value by copy, so there is nothing to synchronize.
    let mut next_conn_id: u64 = 1;
    loop {
        let Ok(permit) = connection_limit.clone().acquire_owned().await else {
            break; // the semaphore is never closed, so this only ends a stuck loop
        };
        let cloned_arguments = arguments.clone();
        match listener.accept().await {
            Ok((stream, addr)) => {
                accept_backoff = ACCEPT_BACKOFF_MIN; // recovered -> reset the backoff
                let conn_id = next_conn_id;
                next_conn_id += 1;
                // The connection's record: it logs the accept line now and the close
                // summary when it is dropped. It is created here and moved into the
                // task, rather than created by the handler, because the runtime can
                // drop a spawned task before its first poll (Ctrl-C right after this
                // accept), and an async fn's body never runs until it is polled. Keep
                // any `.await` out of the code between here and the spawn.
                let conn_stats = ConnStats::accepted(&arguments, conn_id, addr);
                tokio::spawn(async move {
                    // Lent rather than moved: the handler's future, which owns both
                    // `LoggedStream`s, borrows the record, so it is always dropped
                    // first — when the handler returns, and when the runtime drops
                    // this task mid-`.await` at Ctrl-C. That is what makes the
                    // summary the connection's last line.
                    incoming_connection_handle(cloned_arguments, stream, &conn_stats).await;
                    drop(conn_stats); // logs the close summary
                    drop(permit); // then release the slot once the connection is done
                });
            }
            Err(e) => {
                log::error!("Failed to accept incoming connection due to {e}");
                drop(permit); // nothing was accepted, so free the slot

                // Back off before retrying. A persistent error (e.g. file-descriptor
                // exhaustion, where the connection stays in the backlog) would
                // otherwise spin the loop at 100% CPU and flood the log; the delay
                // grows while the error persists and resets once an accept succeeds.
                sleep(accept_backoff).await;
                accept_backoff = next_accept_backoff(accept_backoff);
            }
        }
    }
}

/// Open a connection to the proxy's remote destination for one accepted client.
/// A literal `IP:port` target is dialed directly; a `hostname:port` target is
/// resolved via DNS at this point (once per connection), with tokio trying each
/// resolved address in turn until one connects. A resolution failure surfaces as
/// an `Err` here, handled by the caller exactly like any other connect failure.
async fn connect_to_target(target: &TargetAddr) -> io::Result<tokio_net::TcpStream> {
    match target {
        TargetAddr::Socket(addr) => tokio_net::TcpStream::connect(*addr).await,
        TargetAddr::Named { host, port } => {
            tokio_net::TcpStream::connect((host.as_str(), *port)).await
        }
    }
}

async fn incoming_connection_handle(
    arguments: Arguments,
    source_stream: tokio_net::TcpStream,
    conn_stats: &ConnStats,
) {
    let conn_log = conn_stats.conn_log();
    let (source_stream_read_half, source_stream_write_half) = io::split(LoggedStream::new(
        source_stream,
        get_formatter_by_kind(arguments.formatting, arguments.separator.as_str()),
        DefaultFilter,
        ConsoleLogger::new_unchecked("debug").with_prefix(conn_log.prefix().to_string()),
    ));
    let destination_stream = match connect_to_target(&arguments.remote_addr).await {
        Ok(stream) => stream,
        Err(error) => {
            conn_log.error(format_args!(
                "Failed to connect to destination {}: {error}",
                arguments.remote_addr
            ));
            conn_stats.record_teardown(Teardown::ConnectFailed);
            // Returning drops the source halves, closing the client connection.
            return;
        }
    };
    // For a hostname target, report that the connection was established, appending
    // which resolved address was actually reached when that is available (useful when
    // a name has several records or sits behind DNS-based failover). The `peer_addr()`
    // detail is best-effort: the line is always logged, so a rare `peer_addr()` failure
    // never silently swallows it. (A literal `IP:port` target would just repeat itself,
    // so it is left out.)
    if let TargetAddr::Named { .. } = &arguments.remote_addr {
        let peer_suffix = destination_stream
            .peer_addr()
            .map(|peer| format!(" ({peer})"))
            .unwrap_or_default();
        conn_log.info(format_args!(
            "Connected to destination {}{peer_suffix}",
            arguments.remote_addr
        ));
    }
    // The destination stream carries the same `[#N] ` prefix as the source stream:
    // its Drop/Error/Shutdown records are the connection's lines too, and without
    // the shared tag they would be unattributable.
    let (destination_stream_read_half, destination_stream_write_half) =
        io::split(LoggedStream::new(
            destination_stream,
            get_formatter_by_kind(arguments.formatting, arguments.separator.as_str()),
            RecordKindFilter::new(&[RecordKind::Drop, RecordKind::Error, RecordKind::Shutdown]),
            ConsoleLogger::new_unchecked("debug").with_prefix(conn_log.prefix().to_string()),
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
    //
    // Both relays (and the watchdog) share per-connection state — the `ConnStats`
    // counters behind these handles and, with `--timeout`, the `ActivityClock` — as
    // sub-futures of *this* task: they are composed with `join!`/`select!`, never
    // spawned. That is the premise of the `Relaxed` ordering documented on
    // [`ActivityClock`](idle::ActivityClock) and on `ConnStats`; keep them composed
    // here.
    let client_to_server = conn_stats.direction(Direction::ClientToServer);
    let server_to_client = conn_stats.direction(Direction::ServerToClient);
    match arguments.timeout {
        None => {
            tokio::join!(
                relay(
                    source_stream_read_half,
                    destination_stream_write_half,
                    client_to_server,
                    None,
                ),
                relay(
                    destination_stream_read_half,
                    source_stream_write_half,
                    server_to_client,
                    None,
                ),
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
                        client_to_server,
                        Some(&clock),
                    ),
                    relay(
                        destination_stream_read_half,
                        source_stream_write_half,
                        server_to_client,
                        Some(&clock),
                    ),
                );
            };
            // The idle-close line is logged *inside* the winning branch's future,
            // not in the arm handler: `select!` drops the losing `relays` future —
            // closing the sockets and sending the FIN — before an arm handler
            // would run, so logging in the handler could let a peer observe the
            // close before the line exists. Logging first also orders the line
            // before the streams' shutdown/drop records.
            tokio::select! {
                _ = relays => {}
                _ = async {
                    wait_until_idle(&clock, idle).await;
                    conn_stats.record_teardown(Teardown::IdleTimeout);
                    // The client address makes the line self-correlating even where
                    // the `[#N]` tag is absent (`--no-connection-ids`) or ambiguous
                    // (ids restart at 1 for every proxy run).
                    conn_log.info(format_args!(
                        "Closing idle connection from {} after {seconds}s of inactivity",
                        conn_stats.client_addr()
                    ));
                } => {}
            }
        }
    }
}

/// Copy bytes from `reader` to `writer` until the stream ends or an I/O error
/// occurs, then shut the writer down so the close is forwarded to its peer.
///
/// Each non-empty chunk is counted on this direction's `stats` handle as soon as it
/// is read, and recorded on the shared `activity` clock (when one is provided), so
/// the connection's idle-timeout watchdog can tell that this direction is still
/// moving data. The copy ends when `reader` reaches end-of-stream (`read_buf` yields
/// `Ok(0)`) or a read/write fails; treating a zero-length read as end-of-stream
/// (rather than retrying) is what stops a closed peer from being polled in a tight
/// loop. How it ended is reported to `stats` for the close summary, and the writer
/// is then shut down (a half-close); because the opposite direction is driven to
/// completion independently, any data still in flight there is delivered before
/// the connection closes.
async fn relay<R, W>(
    mut reader: R,
    mut writer: W,
    stats: RelayStats<'_>,
    activity: Option<&ActivityClock>,
) where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buffer = BytesMut::with_capacity(2048);
    let end = loop {
        let read_length = match reader.read_buf(&mut buffer).await {
            Ok(0) => break RelayEnd::Eof,
            Ok(read_length) => read_length,
            Err(error) => break RelayEnd::ReadFailed(error.kind()),
        };
        stats.received(read_length);
        if let Some(activity) = activity {
            activity.record();
        }
        if let Err(error) = writer.write_all(&buffer[0..read_length]).await {
            break RelayEnd::WriteFailed(error.kind());
        }
        buffer.clear();
    };
    stats.finished(end);
    // Forward the end-of-stream to the peer (half-close). Errors are ignored: the
    // writer may already be closed by a failed write or by the peer.
    let _ = writer.shutdown().await;
}
