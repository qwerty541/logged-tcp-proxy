//! The per-connection close summary: the line every connection ends with, its byte
//! counts, the reason it gives on each way a connection can end, and
//! `--no-close-summary`.
//!
//! The rendering and the attribution rule are checked as plain data first. The rest
//! drive real connections through a proxy of their own, so each test's connection
//! is that proxy's `[#1]`, and wait (bounded) for its summary in the shared capture.
//! Each wait spells out the whole expected line up to the duration, keyed by the
//! client's own ephemeral address: a parallel test's line cannot satisfy it, and
//! neither can a stale line from an earlier test whose client port was reused.

use super::helpers::IO_TIMEOUT;
use super::helpers::LOOPBACK;
use super::helpers::TEST_MAX_CONNECTIONS;
use super::helpers::assert_round_trip;
use super::helpers::connect;
use super::helpers::spawn_echo_server;
use super::helpers::spawn_proxy;
use super::helpers::spawn_proxy_configured;
use super::helpers::spawn_proxy_with_timeout;
use super::log_capture::captured_lines;
use super::log_capture::install_capturing_logger;
use super::log_capture::wait_for_line;
use crate::conn::stats::CloseReason;
use crate::conn::stats::Direction;
use crate::conn::stats::Ending;
use crate::conn::stats::Peer;
use crate::conn::stats::RelayEnd;
use crate::conn::stats::Summary;
use std::io;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Bind a one-shot remote on an ephemeral loopback port that serves its first
/// connection with `serve`, and return its address. Unlike the echo server, a test
/// can script exactly how this remote answers and closes.
async fn spawn_remote<F, Fut>(serve: F) -> SocketAddr
where
    F: FnOnce(TcpStream) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind(LOOPBACK)
        .await
        .expect("failed to bind the remote");
    let addr = listener.local_addr().expect("remote local_addr");
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            serve(stream).await;
        }
    });
    addr
}

/// The `[#1]` close summary line expected for `client_addr`, up to (not including)
/// its duration.
fn summary_prefix(
    client_addr: SocketAddr,
    reason: &str,
    client_to_server: u64,
    server_to_client: u64,
) -> String {
    format!(
        "[#1] Closed connection from {client_addr} ({reason}): \
         client -> server {client_to_server} B, server -> client {server_to_client} B in "
    )
}

/// Assert that a summary line ends in its duration: `<secs>.<millis>s`.
fn assert_ends_in_duration(line: &str) {
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    let duration = line
        .rsplit_once(" in ")
        .and_then(|(_, duration)| duration.strip_suffix('s'))
        .and_then(|duration| duration.split_once('.'));
    assert!(
        matches!(duration, Some((secs, millis)) if digits(secs) && millis.len() == 3 && digits(millis)),
        "a close summary must end in `in <secs>.<millis>s`, got {line:?}"
    );
}

/// Wait for this test's close summary with exactly this reason and these counts,
/// check its duration, and return the line.
async fn expect_summary(
    client_addr: SocketAddr,
    reason: &str,
    client_to_server: u64,
    server_to_client: u64,
) -> String {
    let line = wait_for_line(&summary_prefix(
        client_addr,
        reason,
        client_to_server,
        server_to_client,
    ))
    .await;
    assert_ends_in_duration(&line);
    line
}

/// The whole line once, then every reason's wording, then the duration format.
/// The error kinds' text comes from `std`'s `io::ErrorKind` display, so this is
/// also the test that notices if `std` ever rewords it.
#[test]
fn summary_renders_every_close_reason() {
    use io::ErrorKind::BrokenPipe;
    use io::ErrorKind::ConnectionReset;

    let render = |reason, elapsed| {
        Summary {
            client_addr: "127.0.0.1:50512".parse().expect("a valid address"),
            reason,
            client_to_server: 6,
            server_to_client: 10,
            elapsed,
        }
        .to_string()
    };
    assert_eq!(
        render(
            CloseReason::Ended(Ending::FinishedSending(Peer::Client)),
            Duration::from_millis(12)
        ),
        "Closed connection from 127.0.0.1:50512 (client finished sending first): \
         client -> server 6 B, server -> client 10 B in 0.012s"
    );

    for (reason, expected) in [
        (
            CloseReason::Ended(Ending::FinishedSending(Peer::Client)),
            "client finished sending first",
        ),
        (
            CloseReason::Ended(Ending::FinishedSending(Peer::Server)),
            "server finished sending first",
        ),
        (
            CloseReason::Ended(Ending::Error(Peer::Client, ConnectionReset)),
            "client-side error: connection reset",
        ),
        (
            CloseReason::Ended(Ending::Error(Peer::Server, BrokenPipe)),
            "server-side error: broken pipe",
        ),
        (CloseReason::IdleTimeout { after: None }, "idle timeout"),
        (
            CloseReason::IdleTimeout {
                after: Some(Ending::FinishedSending(Peer::Client)),
            },
            "idle timeout after client finished sending",
        ),
        (
            CloseReason::IdleTimeout {
                after: Some(Ending::Error(Peer::Client, ConnectionReset)),
            },
            "idle timeout after client-side error: connection reset",
        ),
        (CloseReason::ConnectFailed, "connect to destination failed"),
        (CloseReason::Interrupted { after: None }, "interrupted"),
        (
            CloseReason::Interrupted {
                after: Some(Ending::FinishedSending(Peer::Server)),
            },
            "interrupted after server finished sending",
        ),
    ] {
        assert_eq!(reason.to_string(), expected, "{reason:?}");
    }

    // Seconds at millisecond resolution, truncated (never rounded up), never scaled
    // to minutes or hours.
    for (elapsed, expected) in [
        (Duration::ZERO, " in 0.000s"),
        (Duration::from_micros(999_999), " in 0.999s"),
        (Duration::from_millis(3_725_004), " in 3725.004s"),
    ] {
        let line = render(CloseReason::ConnectFailed, elapsed);
        assert!(line.ends_with(expected), "{elapsed:?} rendered as {line:?}");
    }
}

/// End-of-stream and a failed read are charged to a direction's sender, a failed
/// write to its receiver — so both operations that can fail on one socket name the
/// same side, whichever direction noticed it first.
#[test]
fn relay_endings_are_charged_to_the_socket_they_happened_on() {
    use io::ErrorKind::ConnectionReset as Reset;

    for (direction, end, expected) in [
        (
            Direction::ClientToServer,
            RelayEnd::Eof,
            Ending::FinishedSending(Peer::Client),
        ),
        (
            Direction::ServerToClient,
            RelayEnd::Eof,
            Ending::FinishedSending(Peer::Server),
        ),
        (
            Direction::ClientToServer,
            RelayEnd::ReadFailed(Reset),
            Ending::Error(Peer::Client, Reset),
        ),
        (
            Direction::ServerToClient,
            RelayEnd::ReadFailed(Reset),
            Ending::Error(Peer::Server, Reset),
        ),
        (
            Direction::ClientToServer,
            RelayEnd::WriteFailed(Reset),
            Ending::Error(Peer::Server, Reset),
        ),
        (
            Direction::ServerToClient,
            RelayEnd::WriteFailed(Reset),
            Ending::Error(Peer::Client, Reset),
        ),
    ] {
        assert_eq!(direction.ending(end), expected, "{direction:?}, {end:?}");
    }
}

/// The client finishes sending first; each direction's bytes are counted on their
/// own. The two directions carry different amounts, so swapped counters fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_finishing_first_is_summarized_with_each_directions_bytes() {
    install_capturing_logger();

    // Answers a 14-byte request with 2 bytes, then drains until the client's close.
    let remote_addr = spawn_remote(|mut stream| async move {
        let mut request = [0u8; 14];
        if stream.read_exact(&mut request).await.is_ok() && stream.write_all(b"OK").await.is_ok() {
            let _ = stream.read_to_end(&mut Vec::new()).await;
        }
    })
    .await;
    let proxy_addr = spawn_proxy(remote_addr).await;

    let mut client = connect(proxy_addr).await;
    let client_addr = client.local_addr().expect("client local_addr");
    timeout(IO_TIMEOUT, client.write_all(b"Hello, MODBUS!"))
        .await
        .expect("write timed out")
        .expect("failed to write the request");
    let mut reply = [0u8; 2];
    timeout(IO_TIMEOUT, client.read_exact(&mut reply))
        .await
        .expect("read timed out")
        .expect("failed to read the reply");
    timeout(IO_TIMEOUT, client.shutdown())
        .await
        .expect("shutdown timed out")
        .expect("failed to shut the client's sending side down");
    timeout(IO_TIMEOUT, client.read_to_end(&mut Vec::new()))
        .await
        .expect("read timed out: the proxy did not forward the remote's close")
        .expect("failed to read to the end");

    expect_summary(client_addr, "client finished sending first", 14, 2).await;
}

/// The server finishes sending first, here without the client sending a byte.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_finishing_first_is_summarized() {
    install_capturing_logger();

    let remote_addr = spawn_remote(|mut stream| async move {
        let _ = stream.write_all(b"bye").await;
        // Dropping the stream closes it: the server has finished sending.
    })
    .await;
    let proxy_addr = spawn_proxy(remote_addr).await;

    let mut client = connect(proxy_addr).await;
    let client_addr = client.local_addr().expect("client local_addr");
    let mut received = Vec::new();
    timeout(IO_TIMEOUT, client.read_to_end(&mut received))
        .await
        .expect("read timed out")
        .expect("failed to read to the end");
    assert_eq!(received, b"bye");
    drop(client);

    expect_summary(client_addr, "server finished sending first", 0, 3).await;
}

/// A server that resets the connection reaches the client as an ordinary close,
/// and the summary is what names the side that failed. The remote resets only
/// after a byte has been relayed: a reset right after the accept can surface as a
/// failed connect instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_reset_is_summarized_as_a_server_side_error() {
    install_capturing_logger();

    let remote_addr = spawn_remote(|mut stream| async move {
        let mut first = [0u8; 1];
        if stream.read_exact(&mut first).await.is_ok() {
            // Closing with a zero linger sends a reset instead of a normal close.
            stream
                .set_zero_linger()
                .expect("failed to arm the remote's reset");
        }
    })
    .await;
    let proxy_addr = spawn_proxy(remote_addr).await;

    let mut client = connect(proxy_addr).await;
    let client_addr = client.local_addr().expect("client local_addr");
    timeout(IO_TIMEOUT, client.write_all(b"x"))
        .await
        .expect("write timed out")
        .expect("failed to write");
    let mut buffer = [0u8; 8];
    let read = timeout(IO_TIMEOUT, client.read(&mut buffer))
        .await
        .expect("read timed out: the proxy did not forward the remote's reset");
    assert_eq!(
        read.expect("the client must see an ordinary close, not a reset"),
        0,
        "the proxy forwards the server's reset to the client as an ordinary close"
    );
    drop(client);

    // Only the side is pinned: the error kind's text is the OS's, through `std`
    // (`connection reset` on Linux), and may differ elsewhere.
    let line = wait_for_line(&format!(
        "[#1] Closed connection from {client_addr} (server-side error: "
    ))
    .await;
    assert!(
        line.contains("): client -> server 1 B, server -> client 0 B in "),
        "unexpected counts in {line:?}"
    );
    assert_ends_in_duration(&line);
}

/// The idle timeout is the reason, the counts survive the relays being dropped
/// mid-flight, and the summary follows the idle-close line.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_timeout_is_summarized_after_the_idle_close_line() {
    install_capturing_logger();

    let echo_addr = spawn_echo_server().await;
    let proxy_addr = spawn_proxy_with_timeout(echo_addr, Some(1)).await;
    let mut client = connect(proxy_addr).await;
    let client_addr = client.local_addr().expect("client local_addr");
    assert_round_trip(&mut client, b"abc").await;

    let summary = expect_summary(client_addr, "idle timeout", 3, 3).await;
    let lines = captured_lines();
    let idle_line =
        format!("[#1] Closing idle connection from {client_addr} after 1s of inactivity");
    let idle_at = lines.iter().position(|line| *line == idle_line);
    let summary_at = lines.iter().position(|line| *line == summary);
    assert!(
        matches!((idle_at, summary_at), (Some(idle), Some(summary)) if idle < summary),
        "the summary must follow the idle-close line; captured: {lines:?}"
    );
}

/// A direction that had already ended is not hidden behind the idle timeout: the
/// client finishes sending, the server stays open without a word, and the timeout
/// then closes the half-closed connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_timeout_after_a_half_close_names_the_side_that_finished() {
    install_capturing_logger();

    let remote_addr = spawn_remote(|mut stream| async move {
        let _ = stream.read_to_end(&mut Vec::new()).await;
        // Keep the connection open, silently, until the test's runtime is dropped.
        std::future::pending::<()>().await;
    })
    .await;
    let proxy_addr = spawn_proxy_with_timeout(remote_addr, Some(1)).await;

    let mut client = connect(proxy_addr).await;
    let client_addr = client.local_addr().expect("client local_addr");
    timeout(IO_TIMEOUT, client.write_all(b"abc"))
        .await
        .expect("write timed out")
        .expect("failed to write");
    timeout(IO_TIMEOUT, client.shutdown())
        .await
        .expect("shutdown timed out")
        .expect("failed to shut the client's sending side down");

    expect_summary(
        client_addr,
        "idle timeout after client finished sending",
        3,
        0,
    )
    .await;
}

/// A failed destination connect still ends with a summary, so every accept line has
/// one; nothing was relayed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_failure_is_summarized() {
    install_capturing_logger();

    // Reserve a port, then release it so nothing is listening there.
    let dead = TcpListener::bind(LOOPBACK)
        .await
        .expect("failed to bind to reserve a dead port");
    let dead_remote_addr = dead.local_addr().expect("dead local_addr");
    drop(dead);

    let proxy_addr = spawn_proxy(dead_remote_addr).await;
    let client = connect(proxy_addr).await;
    let client_addr = client.local_addr().expect("client local_addr");

    expect_summary(client_addr, "connect to destination failed", 0, 0).await;
}

/// Counts add up across many reads in each direction. The payload spans dozens of
/// the relay's 2 KiB chunks, which catches a counter that is overwritten rather
/// than added to, or one that counts buffer capacity rather than bytes read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_chunk_transfers_are_counted_exactly() {
    install_capturing_logger();

    let echo_addr = spawn_echo_server().await;
    let proxy_addr = spawn_proxy(echo_addr).await;
    let client = connect(proxy_addr).await;
    let client_addr = client.local_addr().expect("client local_addr");

    // Written and read concurrently, so the echo never stalls on a full buffer.
    let (mut reader, mut writer) = client.into_split();
    let payload: Vec<u8> = (0..65_536u32).map(|i| (i % 251) as u8).collect();
    let sent = payload.clone();
    let sender = tokio::spawn(async move {
        writer
            .write_all(&sent)
            .await
            .expect("failed to write the payload");
        writer
            .shutdown()
            .await
            .expect("failed to shut the client's sending side down");
    });
    let mut echoed = Vec::new();
    timeout(IO_TIMEOUT, reader.read_to_end(&mut echoed))
        .await
        .expect("read timed out")
        .expect("failed to read the echo");
    sender.await.expect("the sending task panicked");
    assert_eq!(echoed, payload, "the payload must round-trip intact");

    expect_summary(client_addr, "client finished sending first", 65_536, 65_536).await;
}

/// A connection still open when the runtime shuts down — what Ctrl-C does in `main`
/// — is summarized as interrupted, with its counts. This pins the mechanism on
/// every OS; the black-box Ctrl-C case runs on POSIX only.
#[test]
fn connection_open_at_runtime_shutdown_is_summarized_as_interrupted() {
    install_capturing_logger();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("failed to build a runtime");
    let (client, client_addr) = runtime.block_on(async {
        let echo_addr = spawn_echo_server().await;
        let proxy_addr = spawn_proxy_with_timeout(echo_addr, None).await;
        let mut client = connect(proxy_addr).await;
        let client_addr = client.local_addr().expect("client local_addr");
        assert_round_trip(&mut client, b"\xc0\xff\xee\x5a").await;
        // A std socket is not tied to the runtime dropped below, so the client stays
        // open past it.
        let client = client.into_std().expect("failed to convert the client");
        (client, client_addr)
    });

    // Dropping the runtime drops the connection's task mid-relay, and waits until
    // every task is dropped: the summary is already captured when this returns.
    drop(runtime);
    let prefix = summary_prefix(client_addr, "interrupted", 4, 4);
    let lines = captured_lines();
    let summary = lines
        .iter()
        .find(|line| line.starts_with(&prefix))
        .unwrap_or_else(|| panic!("no {prefix:?} line; captured: {lines:?}"));
    assert_ends_in_duration(summary);
    drop(client);
}

/// `--no-close-summary` keeps the summary out of the log, and nothing else.
#[test]
fn no_close_summary_disables_the_summary() {
    install_capturing_logger();

    // An unusual length, so the summary this would log were the flag ignored cannot
    // be mistaken for another test's line.
    let payload = b"summaries are switched off";
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("failed to build a runtime");
    let (client, client_addr) = runtime.block_on(async {
        let echo_addr = spawn_echo_server().await;
        let proxy_addr =
            spawn_proxy_configured(echo_addr, None, TEST_MAX_CONNECTIONS, |arguments| {
                arguments.close_summary = false
            })
            .await;
        let mut client = connect(proxy_addr).await;
        let client_addr = client.local_addr().expect("client local_addr");
        assert_round_trip(&mut client, payload).await;
        let client = client.into_std().expect("failed to convert the client");
        (client, client_addr)
    });

    // As above: once the runtime is dropped, a summary would already be captured.
    drop(runtime);
    let lines = captured_lines();
    assert!(
        lines.contains(&format!("[#1] Incoming connection from {client_addr}")),
        "the accept line must still be logged; captured: {lines:?}"
    );
    let length = payload.len() as u64;
    let summary = summary_prefix(client_addr, "interrupted", length, length);
    assert!(
        !lines.iter().any(|line| line.starts_with(&summary)),
        "--no-close-summary must suppress the summary; captured: {lines:?}"
    );
    drop(client);
}
