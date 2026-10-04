//! The per-connection close summary: the [`ConnStats`] record that logs a
//! connection's accept line when it is created and its one-line close summary when
//! it is dropped, the byte counters both relay directions bump in between, and the
//! vocabulary the summary is worded in.
//!
//! The record is created in [`run_accept_loop`](super::run_accept_loop), before the
//! connection's task is spawned, and lent to
//! [`incoming_connection_handle`](super::incoming_connection_handle) and its two
//! [`relay`](super::relay) calls; why it must live exactly there is explained at
//! those sites. The vocabulary types ([`Summary`], [`CloseReason`], ...) are plain
//! data, so a summary can be rendered, and the attribution rule checked, without
//! any sockets.

use super::logging::ConnLog;
use crate::args::Arguments;
use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

/// One side of a proxied connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Peer {
    /// The peer that connected to the proxy's listener.
    Client,
    /// The destination (`--remote-addr`) the proxy connected to for the client.
    Server,
}

impl fmt::Display for Peer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Peer::Client => "client",
            Peer::Server => "server",
        })
    }
}

/// One of a connection's two relay directions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    /// Bytes read from the client and forwarded to the server: the `<` payload lines.
    ClientToServer,
    /// Bytes read from the server and forwarded to the client: the `>` payload lines.
    ServerToClient,
}

impl Direction {
    /// Charge the way this direction stopped to the side whose socket it happened on.
    ///
    /// End-of-stream and a failed read belong to the direction's *sender*, a failed
    /// write to its *receiver*. Both operations that can fail on one socket therefore
    /// name the same side, whichever direction happened to notice the failure first.
    pub(crate) fn ending(self, end: RelayEnd) -> Ending {
        let (sender, receiver) = match self {
            Direction::ClientToServer => (Peer::Client, Peer::Server),
            Direction::ServerToClient => (Peer::Server, Peer::Client),
        };
        match end {
            RelayEnd::Eof => Ending::FinishedSending(sender),
            RelayEnd::ReadFailed(kind) => Ending::Error(sender, kind),
            RelayEnd::WriteFailed(kind) => Ending::Error(receiver, kind),
        }
    }
}

/// How one relay direction stopped, as [`relay`](super::relay) saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelayEnd {
    /// Reading from the sender reached end-of-stream: it finished sending.
    Eof,
    /// Reading from the sender failed.
    ReadFailed(io::ErrorKind),
    /// Writing to the receiver failed.
    WriteFailed(io::ErrorKind),
}

/// A relay direction's ending, charged to a side by [`Direction::ending`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ending {
    /// That side finished sending. The other side may still have sent data
    /// afterwards: a half-closed connection keeps relaying the other way.
    FinishedSending(Peer),
    /// An I/O error on the proxy's socket to that side, e.g. the side reset the
    /// connection. The OS's own message is on the `!` error line logged before.
    Error(Peer, io::ErrorKind),
}

impl fmt::Display for Ending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ending::FinishedSending(peer) => write!(f, "{peer} finished sending"),
            Ending::Error(peer, kind) => write!(f, "{peer}-side error: {kind}"),
        }
    }
}

/// Why a connection closed: the parenthesized part of its summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloseReason {
    /// Both directions stopped on their own; this is how the first one did.
    Ended(Ending),
    /// `--timeout` closed the connection. `after` is how one direction had already
    /// stopped, if it had (e.g. a half-closed connection that then went quiet).
    IdleTimeout { after: Option<Ending> },
    /// Connecting to the destination (or resolving its name) failed, so nothing was
    /// relayed.
    ConnectFailed,
    /// The proxy was stopped (Ctrl-C) while the connection was open. `after` is how
    /// one direction had already stopped, if it had.
    Interrupted { after: Option<Ending> },
}

impl fmt::Display for CloseReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CloseReason::Ended(ending @ Ending::FinishedSending(_)) => {
                write!(f, "{ending} first")
            }
            CloseReason::Ended(ending) => write!(f, "{ending}"),
            CloseReason::IdleTimeout { after: None } => f.write_str("idle timeout"),
            CloseReason::IdleTimeout {
                after: Some(ending),
            } => write!(f, "idle timeout after {ending}"),
            CloseReason::ConnectFailed => f.write_str("connect to destination failed"),
            CloseReason::Interrupted { after: None } => f.write_str("interrupted"),
            CloseReason::Interrupted {
                after: Some(ending),
            } => write!(f, "interrupted after {ending}"),
        }
    }
}

/// An ending the handler caused itself. It takes precedence over the relays' own
/// endings when the summary picks its reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Teardown {
    /// The idle-timeout watchdog closed the connection.
    IdleTimeout,
    /// Connecting to the destination failed, so no relay ever ran.
    ConnectFailed,
}

/// A connection's close summary as plain data. Its [`Display`](fmt::Display) is the
/// logged message, without the `[#N] ` tag that [`ConnLog`] adds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Summary {
    pub(crate) client_addr: SocketAddr,
    pub(crate) reason: CloseReason,
    /// Bytes read from the client: exactly the bytes of the connection's `<` lines.
    pub(crate) client_to_server: u64,
    /// Bytes read from the server: the bytes of the `>` lines, plus at most one
    /// chunk (2 KiB) read but never delivered when a write to the client failed or
    /// was cut short.
    pub(crate) server_to_client: u64,
    /// Time since the proxy accepted the connection.
    pub(crate) elapsed: Duration,
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Seconds at millisecond resolution, truncated, whatever `--precision` says:
        // that option is about timestamps, and one fixed format is easier to read
        // and to search for.
        write!(
            f,
            "Closed connection from {} ({}): client -> server {} B, server -> client {} B in {}.{:03}s",
            self.client_addr,
            self.reason,
            self.client_to_server,
            self.server_to_client,
            self.elapsed.as_secs(),
            self.elapsed.subsec_millis(),
        )
    }
}

/// One relay direction's counters.
#[derive(Default)]
struct DirectionCounters {
    /// Bytes read from this direction's sender.
    bytes: AtomicU64,
    /// Whether this direction's relay has stopped.
    finished: AtomicBool,
}

/// The record of one accepted connection: it logs the connection's accept line when
/// created, collects what the close summary reports while the connection is open,
/// and logs that summary when dropped.
///
/// Exactly one is created per accepted connection, by [`accepted`](Self::accepted)
/// in the accept loop. The accept line and the summary come from the same value, so
/// every `Incoming connection` line is matched by exactly one `Closed connection`
/// line, on every way a connection can end — including Ctrl-C, but not a killed
/// process (e.g. SIGTERM), which runs no destructors at all.
///
/// It owns the connection's [`ConnLog`], which the handler borrows through
/// [`conn_log`](Self::conn_log): the summary is one of the connection's lines, so it
/// carries the same tag as all the others.
pub(super) struct ConnStats {
    conn_log: ConnLog,
    client_addr: SocketAddr,
    /// `std`'s clock rather than tokio's: `Drop` may run while the runtime is being
    /// torn down.
    accepted: Instant,
    /// `false` with `--no-close-summary`. Everything is still counted (two relaxed
    /// atomic operations per chunk), only the line is not logged.
    summary_enabled: bool,
    // `Relaxed`, on the premise documented on `super::idle::ActivityClock`: the relays
    // that update these are `join!`/`select!` sub-futures of the connection's one
    // task, never spawned. The values are read only in `Drop`, either in that same
    // task once the handler has returned, or when the runtime drops the task at
    // shutdown, possibly on another worker thread — and the runtime's own task-state
    // transitions (acquire/release) order the last poll's updates before that drop.
    // Atomics rather than `Cell` only because a `&ConnStats` is held across `.await`s
    // and the task must stay `Send` for `tokio::spawn`.
    client_to_server: DirectionCounters,
    server_to_client: DirectionCounters,
    /// How the first direction to stop ended. Set once, by whichever stops first.
    first_end: OnceLock<Ending>,
    /// An ending the handler caused itself; it wins over `first_end`.
    teardown: OnceLock<Teardown>,
}

impl ConnStats {
    /// Start the record of connection `conn_id`, just accepted from `client_addr`:
    /// mint its [`ConnLog`] (honouring `--no-connection-ids`), log its accept line
    /// and start its clock.
    pub(super) fn accepted(arguments: &Arguments, conn_id: u64, client_addr: SocketAddr) -> Self {
        let conn_log = ConnLog::new(arguments, conn_id);
        conn_log.info(format_args!("Incoming connection from {client_addr}"));
        Self {
            conn_log,
            client_addr,
            accepted: Instant::now(),
            summary_enabled: arguments.close_summary,
            client_to_server: DirectionCounters::default(),
            server_to_client: DirectionCounters::default(),
            first_end: OnceLock::new(),
            teardown: OnceLock::new(),
        }
    }

    /// The connection's logger, for every line the handler logs about it.
    pub(super) fn conn_log(&self) -> &ConnLog {
        &self.conn_log
    }

    /// The client's address, as named on the accept line.
    pub(super) fn client_addr(&self) -> SocketAddr {
        self.client_addr
    }

    /// The handle one relay direction counts and reports its ending through.
    pub(super) fn direction(&self, direction: Direction) -> RelayStats<'_> {
        RelayStats {
            stats: self,
            direction,
        }
    }

    /// Record that the handler ended the connection itself. Only the first record
    /// counts (a connection is never ended twice).
    pub(super) fn record_teardown(&self, teardown: Teardown) {
        let _ = self.teardown.set(teardown);
    }

    fn counters(&self, direction: Direction) -> &DirectionCounters {
        match direction {
            Direction::ClientToServer => &self.client_to_server,
            Direction::ServerToClient => &self.server_to_client,
        }
    }

    /// Why the connection closed. A teardown the handler recorded wins. Otherwise,
    /// if both directions stopped, it is how the first one did. Otherwise the task
    /// was dropped with a direction still running, which only the runtime shutting
    /// down at Ctrl-C does (to a task mid-relay, or to one that never ran at all).
    /// An overriding reason keeps a direction's earlier ending as its `after`, so a
    /// side that failed or finished first is never hidden behind it.
    fn close_reason(&self) -> CloseReason {
        let first_end = self.first_end.get().copied();
        match self.teardown.get() {
            Some(Teardown::ConnectFailed) => CloseReason::ConnectFailed,
            Some(Teardown::IdleTimeout) => CloseReason::IdleTimeout { after: first_end },
            None => match first_end {
                Some(ending) if self.both_finished() => CloseReason::Ended(ending),
                after => CloseReason::Interrupted { after },
            },
        }
    }

    fn both_finished(&self) -> bool {
        [Direction::ClientToServer, Direction::ServerToClient]
            .into_iter()
            .all(|direction| self.counters(direction).finished.load(Ordering::Relaxed))
    }

    fn summary(&self) -> Summary {
        Summary {
            client_addr: self.client_addr,
            reason: self.close_reason(),
            client_to_server: self.client_to_server.bytes.load(Ordering::Relaxed),
            server_to_client: self.server_to_client.bytes.load(Ordering::Relaxed),
            elapsed: self.accepted.elapsed(),
        }
    }
}

impl Drop for ConnStats {
    fn drop(&mut self) {
        if self.summary_enabled {
            let summary = self.summary();
            self.conn_log.info(format_args!("{summary}"));
        }
    }
}

/// One relay direction's handle on its connection's [`ConnStats`].
#[derive(Clone, Copy)]
pub(super) struct RelayStats<'a> {
    stats: &'a ConnStats,
    direction: Direction,
}

impl RelayStats<'_> {
    /// Count a chunk just read from this direction's sender, before it is forwarded.
    pub(super) fn received(self, bytes: usize) {
        self.stats
            .counters(self.direction)
            .bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// Record how this direction stopped. The first direction to stop is the one the
    /// summary reports.
    pub(super) fn finished(self, end: RelayEnd) {
        let _ = self.stats.first_end.set(self.direction.ending(end));
        self.stats
            .counters(self.direction)
            .finished
            .store(true, Ordering::Relaxed);
    }
}
