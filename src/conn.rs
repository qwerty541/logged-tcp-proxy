use crate::args::Arguments;
use crate::args::TargetAddr;
use crate::args::get_formatter_by_kind;
use bytes::BytesMut;
use logged_stream::ConsoleLogger;
use logged_stream::DefaultFilter;
use logged_stream::LoggedStream;
use logged_stream::RecordKind;
use logged_stream::RecordKindFilter;
use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::io::{self};
use tokio::net as tokio_net;
use tokio::sync::Semaphore;
use tokio::time::Instant;
use tokio::time::sleep;
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
                let conn_log = ConnLog::new(&arguments, conn_id);
                conn_log.info(format_args!("Incoming connection from {addr}"));
                tokio::spawn(async move {
                    incoming_connection_handle(cloned_arguments, stream, conn_log, addr).await;
                    drop(permit); // release the slot once the connection is done
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

/// Opening delimiter of a connection's `[#N] ` console tag.
///
/// The tag's grammar lives here rather than being spelled out at each site that
/// produces or parses it, so a change to the shape cannot leave a parser quietly
/// matching nothing (see `strip_conn_tag` in [`log_capture`](crate::tests::log_capture)).
pub(crate) const CONN_TAG_OPEN: &str = "[#";
/// Closing delimiter of a connection's `[#N] ` console tag. The trailing space is
/// part of it: [`ConsoleLogger`] renders the prefix verbatim, immediately before
/// the record-kind character, with no separator of its own.
pub(crate) const CONN_TAG_CLOSE: &str = "] ";

/// Everything one proxied connection logs, carrying that connection's id tag.
///
/// The tag is `"[#N] "` (see [`CONN_TAG_OPEN`] / [`CONN_TAG_CLOSE`]), or an empty
/// string when `--no-connection-ids` disabled the tags — an empty prefix renders
/// byte-for-byte like no prefix at all, so the disabled path reproduces the
/// untagged output exactly through the same code.
///
/// Every per-connection line goes through this type: the lifecycle lines via
/// [`log`](Self::log) / [`trace`](Self::trace) / [`debug`](Self::debug) /
/// [`info`](Self::info) / [`warn`](Self::warn) / [`error`](Self::error), and both
/// `LoggedStream`s' console records via [`prefix`](Self::prefix). That is what keeps
/// the tag from being forgotten — a new per-connection line cannot be logged without
/// one, so the "every line of a connection is attributable" guarantee is structural
/// rather than a convention each future call site has to remember.
struct ConnLog {
    prefix: String,
}

impl ConnLog {
    /// Build the logger for connection `conn_id`, honouring `--no-connection-ids`.
    fn new(arguments: &Arguments, conn_id: u64) -> Self {
        Self {
            prefix: if arguments.connection_ids {
                format!("{CONN_TAG_OPEN}{conn_id}{CONN_TAG_CLOSE}")
            } else {
                String::new()
            },
        }
    }

    /// The connection's tag, for [`ConsoleLogger::with_prefix`].
    fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Log a line with the connection's tag at the given level. The `message`
    /// argument is a `format_args!`-style `fmt::Arguments` value, so the caller
    /// can use `{}`-style formatting without allocating a `String`.
    #[allow(dead_code)]
    fn log(&self, level: log::Level, message: fmt::Arguments<'_>) {
        log::log!(level, "{}{message}", self.prefix);
    }

    /// Log one of the connection's debug lines, tagged, at the `trace` level.
    #[allow(dead_code)]
    fn trace(&self, message: fmt::Arguments<'_>) {
        log::trace!("{}{message}", self.prefix);
    }

    /// Log one of the connection's debug lines, tagged, at the `debug` level.
    #[allow(dead_code)]
    fn debug(&self, message: fmt::Arguments<'_>) {
        log::debug!("{}{message}", self.prefix);
    }

    /// Log one of the connection's lifecycle lines, tagged, at the `info` level.
    fn info(&self, message: fmt::Arguments<'_>) {
        log::info!("{}{message}", self.prefix);
    }

    /// Log one of the connection's warning lines, tagged, at the `warn` level.
    #[allow(dead_code)]
    fn warn(&self, message: fmt::Arguments<'_>) {
        log::warn!("{}{message}", self.prefix);
    }

    /// Log one of the connection's failure lines, tagged, at the `error` level.
    fn error(&self, message: fmt::Arguments<'_>) {
        log::error!("{}{message}", self.prefix);
    }
}

async fn incoming_connection_handle(
    arguments: Arguments,
    source_stream: tokio_net::TcpStream,
    conn_log: ConnLog,
    client_addr: SocketAddr,
) {
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
                    // The client address makes the line self-correlating even where
                    // the `[#N]` tag is absent (`--no-connection-ids`) or ambiguous
                    // (ids restart at 1 for every proxy run).
                    conn_log.info(format_args!(
                        "Closing idle connection from {client_addr} after {seconds}s of inactivity"
                    ));
                } => {}
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
    // `Relaxed` is deliberate. The relays and the watchdog that touch this are
    // cooperatively-scheduled sub-futures of a *single* task (composed with
    // `join!`/`select!`, not separate spawns — note they borrow `&self`), so they
    // never access it from two threads at once. It is also a self-contained
    // timestamp that guards no other memory, so there is nothing for Acquire/Release
    // to publish; single-location coherence is the whole requirement, and the
    // watchdog re-reads after sleeping whole seconds, far longer than any store can
    // take to become visible.
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
        // `idle` is the `--timeout` value, which `args::Arguments` range-validates to
        // at most ~100 years, so this `Instant + Duration` can never overflow the
        // monotonic clock (which would otherwise panic).
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
