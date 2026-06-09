//! In-crate integration tests for the TCP proxy relay.
//!
//! These run in-process via `cargo test` (no `lib` target, nothing exposed
//! publicly or on docs.rs) and behave identically in CI and locally. Each test
//! brings its own pure-Tokio echo server, so there are no external dependencies
//! (python, netcat, ...) and no network access beyond `127.0.0.1`. All listeners
//! bind to ephemeral ports (`127.0.0.1:0`) and all I/O is bounded by a timeout,
//! so the tests are deterministic and never collide across parallel jobs.

use crate::args::Arguments;
use crate::args::LoggingLevel;
use crate::args::PayloadFormatingKind;
use crate::args::TimestampPrecision;
use crate::conn::run_accept_loop;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::task::JoinHandle;
use tokio::time::timeout;

/// Upper bound for any single network operation in the tests. Generous enough to
/// avoid flakiness on a loaded CI runner, small enough to fail fast on a hang.
const IO_TIMEOUT: Duration = Duration::from_secs(10);

/// The loopback host literal. Always `127.0.0.1` (never `localhost`, which can
/// resolve to IPv6 on Windows).
const LOOPBACK: &str = "127.0.0.1:0";

/// Build proxy `Arguments` pointing at `remote_addr`, with logging silenced so
/// test output stays clean.
fn test_arguments(bind_listener_addr: SocketAddr, remote_addr: SocketAddr) -> Arguments {
    Arguments {
        level: LoggingLevel::Off,
        bind_listener_addr,
        remote_addr,
        timeout: 5,
        formatting: PayloadFormatingKind::LowerHex,
        separator: ":".to_string(),
        precision: TimestampPrecision::Seconds,
    }
}

/// Spawn a minimal echo server on an ephemeral loopback port. Returns the bound
/// address; the server runs until the test's runtime is dropped.
async fn spawn_echo_server() -> SocketAddr {
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

/// Bind a proxy listener on an ephemeral loopback port and start its accept
/// loop. Returns the proxy's bound address and the accept-loop task handle (abort
/// it to stop the proxy).
async fn spawn_proxy(remote_addr: SocketAddr) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind(LOOPBACK)
        .await
        .expect("failed to bind proxy");
    let addr = listener.local_addr().expect("proxy local_addr");
    let handle = tokio::spawn(run_accept_loop(listener, test_arguments(addr, remote_addr)));
    (addr, handle)
}

/// Connect a client to `addr`, bounded by [`IO_TIMEOUT`].
async fn connect(addr: SocketAddr) -> TcpStream {
    timeout(IO_TIMEOUT, TcpStream::connect(addr))
        .await
        .expect("connect timed out")
        .expect("failed to connect")
}

/// Write `payload` then read exactly `payload.len()` bytes back, asserting the
/// echoed bytes match. Each operation is bounded by [`IO_TIMEOUT`].
async fn assert_round_trip(client: &mut TcpStream, payload: &[u8]) {
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

/// A payload sent by a client is forwarded to the remote and the remote's
/// response is forwarded back, unchanged.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relays_payload_through_remote() {
    let echo_addr = spawn_echo_server().await;
    let (proxy_addr, proxy) = spawn_proxy(echo_addr).await;

    let mut client = connect(proxy_addr).await;
    assert_round_trip(&mut client, b"Hello, MODBUS!").await;

    proxy.abort();
}

/// The relay keeps forwarding across many sequential request/response cycles on a
/// single connection (not just the first read).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relays_multiple_sequential_messages() {
    let echo_addr = spawn_echo_server().await;
    let (proxy_addr, proxy) = spawn_proxy(echo_addr).await;

    let mut client = connect(proxy_addr).await;
    for round in 0..16u8 {
        assert_round_trip(&mut client, &[round; 32]).await;
    }

    proxy.abort();
}

/// Many clients connected at once are each proxied independently and correctly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn handles_multiple_concurrent_clients() {
    let echo_addr = spawn_echo_server().await;
    let (proxy_addr, proxy) = spawn_proxy(echo_addr).await;

    let mut clients = Vec::new();
    for i in 0..16u8 {
        clients.push(tokio::spawn(async move {
            let mut client = connect(proxy_addr).await;
            // A distinct, per-client payload so a mix-up between connections would
            // be caught by the round-trip assertion.
            assert_round_trip(&mut client, &[i; 64]).await;
        }));
    }

    for client in clients {
        client.await.expect("client task panicked");
    }

    proxy.abort();
}
