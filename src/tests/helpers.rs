//! Shared test scaffolding: the timing/limit constants, the `Arguments` builder,
//! the echo-server and proxy spawners, and the client-side round-trip helpers used
//! across the test submodules.

use crate::args::Arguments;
use crate::args::LoggingLevel;
use crate::args::PayloadFormattingKind;
use crate::args::TargetAddr;
use crate::args::TimestampPrecision;
use crate::conn::run_accept_loop;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Upper bound for any single network operation in the tests. Generous enough to
/// avoid flakiness on a loaded CI runner, small enough to fail fast on a hang.
pub(super) const IO_TIMEOUT: Duration = Duration::from_secs(10);

/// Loopback bind address using an ephemeral port (`127.0.0.1:0`). Always `127.0.0.1`
/// (never `localhost`, which can resolve to IPv6 on Windows).
pub(super) const LOOPBACK: &str = "127.0.0.1:0";

/// A generous connection cap used by the helpers, so tests that open several
/// concurrent connections are not throttled by the limit under test.
pub(super) const TEST_MAX_CONNECTIONS: u32 = 512;

/// Build proxy `Arguments` pointing at `remote_addr`, with logging silenced so
/// test output stays clean.
pub(super) fn test_arguments(
    bind_listener_addr: SocketAddr,
    remote_addr: SocketAddr,
    timeout: Option<u64>,
    max_connections: u32,
) -> Arguments {
    Arguments {
        level: LoggingLevel::Off,
        bind_listener_addr,
        remote_addr: remote_addr.into(),
        timeout,
        max_connections,
        // Irrelevant to the relay path under test: the worker-thread count only
        // shapes the runtime built in `main`, which these tests do not exercise.
        threads: 4,
        formatting: PayloadFormattingKind::LowerHex,
        separator: ":".to_string(),
        precision: TimestampPrecision::Seconds,
    }
}

/// Spawn a minimal echo server on an ephemeral loopback port. Returns the bound
/// address; the server runs until the test's runtime is dropped.
pub(super) async fn spawn_echo_server() -> SocketAddr {
    let listener = TcpListener::bind(LOOPBACK)
        .await
        .expect("failed to bind echo server");
    let addr = listener.local_addr().expect("echo server local_addr");

    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                loop {
                    match stream.read(&mut buffer).await {
                        Ok(0) | Err(_) => break,
                        Ok(read_length) => {
                            if stream.write_all(&buffer[0..read_length]).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
    });

    addr
}

/// Spawn an echo server bound via the *name* `localhost`, rather than the literal
/// `127.0.0.1` the other helpers use, and return its port. Binding through the same
/// name the proxy resolves keeps the address family in sync on platforms where
/// `localhost` is IPv6 (e.g. Windows), and the proxy's connect tries every resolved
/// address anyway, so v4/v6 ordering never matters. Used by the tests that need a
/// remote reachable *by name*.
pub(super) async fn spawn_localhost_echo_server() -> u16 {
    let listener = TcpListener::bind(("localhost", 0))
        .await
        .expect("failed to bind echo server on localhost");
    let port = listener
        .local_addr()
        .expect("echo server local_addr")
        .port();

    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                loop {
                    match stream.read(&mut buffer).await {
                        Ok(0) | Err(_) => break,
                        Ok(read_length) => {
                            if stream.write_all(&buffer[0..read_length]).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
    });

    port
}

/// Bind a proxy listener on an ephemeral loopback port and start its accept loop
/// in the background. Returns the proxy's bound address. Uses a bounded idle
/// timeout so a stuck test fails fast rather than hanging.
///
/// There is no handle to stop it: the accept loop is intentionally infinite (a
/// proxy keeps listening), and per-connection cleanup is automatic — each
/// connection's relay tasks and sockets are released when it closes. The accept
/// loop itself is cancelled when the test's runtime is dropped at the end of the
/// test, which also releases the ephemeral port.
pub(super) async fn spawn_proxy(remote_addr: SocketAddr) -> SocketAddr {
    spawn_proxy_full(
        remote_addr,
        Some(IO_TIMEOUT.as_secs()),
        TEST_MAX_CONNECTIONS,
    )
    .await
}

/// Like [`spawn_proxy`] but with an explicit `--timeout` value, where `None` is
/// the default behaviour of no idle timeout.
pub(super) async fn spawn_proxy_with_timeout(
    remote_addr: SocketAddr,
    timeout: Option<u64>,
) -> SocketAddr {
    spawn_proxy_full(remote_addr, timeout, TEST_MAX_CONNECTIONS).await
}

/// Like [`spawn_proxy`] but with an explicit `--max-connections` cap.
pub(super) async fn spawn_proxy_with_limit(
    remote_addr: SocketAddr,
    max_connections: u32,
) -> SocketAddr {
    spawn_proxy_full(remote_addr, Some(IO_TIMEOUT.as_secs()), max_connections).await
}

async fn spawn_proxy_full(
    remote_addr: SocketAddr,
    timeout: Option<u64>,
    max_connections: u32,
) -> SocketAddr {
    let listener = TcpListener::bind(LOOPBACK)
        .await
        .expect("failed to bind proxy");
    let addr = listener.local_addr().expect("proxy local_addr");
    tokio::spawn(run_accept_loop(
        listener,
        test_arguments(addr, remote_addr, timeout, max_connections),
    ));
    addr
}

/// Like [`spawn_proxy`] but with an explicit remote target, so a test can point
/// the proxy at a `hostname:port` (resolved at connect time) instead of a literal
/// address. Reuses the same ephemeral-port + auto-cleanup setup as the others.
pub(super) async fn spawn_proxy_with_target(remote: TargetAddr) -> SocketAddr {
    let listener = TcpListener::bind(LOOPBACK)
        .await
        .expect("failed to bind proxy");
    let addr = listener.local_addr().expect("proxy local_addr");
    let mut arguments =
        test_arguments(addr, addr, Some(IO_TIMEOUT.as_secs()), TEST_MAX_CONNECTIONS);
    arguments.remote_addr = remote;
    tokio::spawn(run_accept_loop(listener, arguments));
    addr
}

/// Connect a client to `addr`, bounded by [`IO_TIMEOUT`].
pub(super) async fn connect(addr: SocketAddr) -> TcpStream {
    timeout(IO_TIMEOUT, TcpStream::connect(addr))
        .await
        .expect("connect timed out")
        .expect("failed to connect")
}

/// Write `payload` then read exactly `payload.len()` bytes back, asserting the
/// echoed bytes match. Each operation is bounded by [`IO_TIMEOUT`].
pub(super) async fn assert_round_trip(client: &mut TcpStream, payload: &[u8]) {
    timeout(IO_TIMEOUT, client.write_all(payload))
        .await
        .expect("write timed out")
        .expect("failed to write to proxy");

    let mut received = vec![0u8; payload.len()];
    timeout(IO_TIMEOUT, client.read_exact(&mut received))
        .await
        .expect("read timed out")
        .expect("failed to read echo back through proxy");

    assert_eq!(
        received, payload,
        "payload must round-trip through the proxy"
    );
}
