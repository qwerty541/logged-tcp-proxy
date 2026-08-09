//! The accept loop itself: the `--max-connections` semaphore that bounds how many
//! connections are served at once, and the backoff that keeps a persistently
//! failing `accept()` from busy-spinning.

use super::helpers::IO_TIMEOUT;
use super::helpers::assert_round_trip;
use super::helpers::connect;
use super::helpers::spawn_echo_server;
use super::helpers::spawn_proxy_with_limit;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::time::timeout;

/// `--max-connections` bounds how many connections are handled at once: while the
/// cap is reached, a further connection is accepted by the kernel but not served
/// until a slot frees.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn caps_concurrent_connections() {
    let echo_addr = spawn_echo_server().await;
    let proxy_addr = spawn_proxy_with_limit(echo_addr, 1).await; // one connection at a time

    // The first client takes the only slot and keeps it (stays open).
    let mut first = connect(proxy_addr).await;
    assert_round_trip(&mut first, b"first").await;

    // The second client connects (the kernel completes the handshake into the
    // backlog), but the proxy is at capacity, so it is NOT served yet: its data is
    // not relayed back within a short window.
    let mut second = connect(proxy_addr).await;
    timeout(IO_TIMEOUT, second.write_all(b"second"))
        .await
        .expect("write timed out")
        .expect("failed to write");
    let mut buffer = [0u8; 16];
    let while_capped = timeout(Duration::from_millis(500), second.read(&mut buffer)).await;
    assert!(
        while_capped.is_err(),
        "the second connection must not be served while the cap is reached"
    );

    // Free the slot; the second connection is now served and its data round-trips.
    drop(first);
    let read_length = timeout(IO_TIMEOUT, second.read(&mut buffer))
        .await
        .expect("read timed out after a slot freed")
        .expect("failed to read");
    assert_eq!(
        &buffer[0..read_length],
        b"second",
        "the second connection is served once a slot frees"
    );
}

/// The accept-error backoff grows while errors persist but never exceeds the cap,
/// so a persistent `accept()` failure can't busy-spin the accept loop.
#[test]
fn accept_backoff_grows_and_caps() {
    use crate::conn::ACCEPT_BACKOFF_MAX;
    use crate::conn::ACCEPT_BACKOFF_MIN;
    use crate::conn::next_accept_backoff;

    assert!(ACCEPT_BACKOFF_MIN < ACCEPT_BACKOFF_MAX);
    let mut delay = ACCEPT_BACKOFF_MIN;
    let mut previous = delay;
    for _ in 0..16 {
        delay = next_accept_backoff(delay);
        assert!(
            delay >= previous,
            "the backoff must not shrink while errors persist"
        );
        assert!(delay <= ACCEPT_BACKOFF_MAX, "the backoff is capped");
        previous = delay;
    }
    assert_eq!(
        delay, ACCEPT_BACKOFF_MAX,
        "the backoff reaches and holds at the cap"
    );
}
