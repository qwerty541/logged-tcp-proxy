//! The whole-connection idle timeout: it fires when both directions are silent,
//! is absent by default, and is never triggered while either direction is active.

use super::helpers::IO_TIMEOUT;
use super::helpers::LOOPBACK;
use super::helpers::assert_round_trip;
use super::helpers::connect;
use super::helpers::spawn_echo_server;
use super::helpers::spawn_proxy_with_timeout;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::time::sleep;
use tokio::time::timeout;

/// When `--timeout` is set, a connection that is idle in BOTH directions is torn
/// down once the timeout elapses: the client observes the connection close.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_connection_times_out_when_timeout_is_set() {
    let echo_addr = spawn_echo_server().await;
    let proxy_addr = spawn_proxy_with_timeout(echo_addr, Some(1)).await;

    // Neither the client nor the (echo) remote sends anything, so both directions
    // stay idle; after ~1s the watchdog tears the connection down.
    let mut client = connect(proxy_addr).await;
    let mut buffer = [0u8; 16];
    let result = timeout(IO_TIMEOUT, client.read(&mut buffer))
        .await
        .expect("client read timed out: the idle timeout did not fire");
    match result {
        Ok(0) | Err(_) => {} // clean end-of-stream or a reset: the connection closed
        Ok(n) => panic!("expected the idle connection to close, but read {n} bytes"),
    }
}

/// Without `--timeout` (the default), an idle connection is NOT torn down: it stays
/// open and still relays after a period of inactivity longer than the timeout used
/// by [`idle_connection_times_out_when_timeout_is_set`].
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_connection_stays_open_without_timeout() {
    let echo_addr = spawn_echo_server().await;
    let proxy_addr = spawn_proxy_with_timeout(echo_addr, None).await;

    let mut client = connect(proxy_addr).await;
    // Stay idle longer than the 1s timeout the sibling test uses; with no timeout
    // the connection must remain usable.
    sleep(Duration::from_millis(1500)).await;
    assert_round_trip(&mut client, b"still alive").await;
}

/// The idle timeout is whole-connection, not per-direction: while the remote keeps
/// the reverse (remote -> client) direction active, the forward (client -> remote)
/// direction is NOT torn down even though it stays idle longer than the timeout, so
/// a message the client eventually sends still reaches the remote.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_reverse_direction_keeps_forward_direction_open() {
    let listener = TcpListener::bind(LOOPBACK)
        .await
        .expect("failed to bind remote");
    let remote_addr = listener.local_addr().expect("remote local_addr");
    // The remote ticks at the client (keeping remote -> client active) for longer
    // than the timeout while concurrently reading the client's eventual message and
    // echoing it back, so the test can observe that it arrived.
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("remote accept");
        let (mut read_half, mut write_half) = stream.into_split();
        let reader = tokio::spawn(async move {
            let mut buffer = [0u8; 64];
            let read_length = read_half.read(&mut buffer).await.unwrap_or(0);
            buffer[0..read_length].to_vec()
        });
        for _ in 0..5 {
            sleep(Duration::from_millis(400)).await;
            if write_half.write_all(b".").await.is_err() {
                break;
            }
        }
        let from_client = reader.await.unwrap_or_default();
        let _ = write_half.write_all(&from_client).await;
        let _ = write_half.shutdown().await;
    });

    let proxy_addr = spawn_proxy_with_timeout(remote_addr, Some(1)).await;
    let mut client = connect(proxy_addr).await;

    // Stay idle on the forward direction past the 1s timeout; the remote's ticks
    // keep the connection alive. Then send a marker and confirm it round-trips,
    // proving the forward direction was never closed.
    sleep(Duration::from_millis(1200)).await;
    timeout(IO_TIMEOUT, client.write_all(b"PING"))
        .await
        .expect("write timed out")
        .expect("failed to send marker");

    let mut received = Vec::new();
    timeout(IO_TIMEOUT, client.read_to_end(&mut received))
        .await
        .expect("read timed out")
        .expect("failed to read");
    assert!(
        received.windows(4).any(|window| window == b"PING"),
        "the forward direction must stay open while the reverse direction is active"
    );
}
