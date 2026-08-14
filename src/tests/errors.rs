//! Failure paths that must not panic: an unavailable listen address and an
//! unreachable remote.

use super::helpers::IO_TIMEOUT;
use super::helpers::LOOPBACK;
use super::helpers::TEST_MAX_CONNECTIONS;
use super::helpers::connect;
use super::helpers::spawn_proxy;
use super::helpers::test_arguments;
use crate::conn::initialize_tcp_listener;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::time::timeout;

/// Binding the listener to an address that is already in use returns an error
/// instead of panicking, so the binary can exit cleanly on a fatal startup error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bind_failure_returns_error() {
    // Hold an active listener so the proxy's bind to the same address fails.
    let occupier = TcpListener::bind(LOOPBACK)
        .await
        .expect("failed to bind occupier");
    let in_use_addr = occupier.local_addr().expect("occupier local_addr");

    // `remote_addr` is irrelevant: the bind fails before any connection is served.
    let result = initialize_tcp_listener(test_arguments(
        in_use_addr,
        in_use_addr,
        None,
        TEST_MAX_CONNECTIONS,
    ))
    .await;

    assert!(
        result.is_err(),
        "binding to an in-use address should return an error, not panic"
    );
}

/// When the remote is unreachable, the proxy must not panic: it logs the failure
/// and closes the already-accepted client connection cleanly (the client's read
/// returns end-of-stream rather than hanging).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreachable_remote_closes_client_cleanly() {
    // Reserve a port, then release it so nothing is listening there.
    let dead = TcpListener::bind(LOOPBACK)
        .await
        .expect("failed to bind to reserve a dead port");
    let dead_remote_addr = dead.local_addr().expect("dead local_addr");
    drop(dead);

    let proxy_addr = spawn_proxy(dead_remote_addr).await;

    let mut client = connect(proxy_addr).await;
    let mut buffer = [0u8; 16];
    let read_length = timeout(IO_TIMEOUT, client.read(&mut buffer))
        .await
        .expect("client read timed out: proxy did not close the connection after a failed remote connect")
        .expect("client read errored");
    assert_eq!(
        read_length, 0,
        "expected end-of-stream after the proxy failed to reach the remote"
    );
}
