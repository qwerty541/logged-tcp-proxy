//! Core relaying behavior: payloads round-trip, many messages and many clients
//! are handled, and both directions flow at once.

use super::helpers::IO_TIMEOUT;
use super::helpers::LOOPBACK;
use super::helpers::assert_round_trip;
use super::helpers::connect;
use super::helpers::spawn_echo_server;
use super::helpers::spawn_proxy;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::time::timeout;

/// A payload sent by a client is forwarded to the remote and the remote's
/// response is forwarded back, unchanged.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relays_payload_through_remote() {
    let echo_addr = spawn_echo_server().await;
    let proxy_addr = spawn_proxy(echo_addr).await;

    let mut client = connect(proxy_addr).await;
    assert_round_trip(&mut client, b"Hello, MODBUS!").await;
}

/// The relay keeps forwarding across many sequential request/response cycles on a
/// single connection (not just the first read).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relays_multiple_sequential_messages() {
    let echo_addr = spawn_echo_server().await;
    let proxy_addr = spawn_proxy(echo_addr).await;

    let mut client = connect(proxy_addr).await;
    for round in 0..16u8 {
        assert_round_trip(&mut client, &[round; 32]).await;
    }
}

/// Many clients connected at once are each proxied independently and correctly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn handles_multiple_concurrent_clients() {
    let echo_addr = spawn_echo_server().await;
    let proxy_addr = spawn_proxy(echo_addr).await;

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
}

/// Both directions are relayed concurrently: a large payload flowing
/// client -> remote does not block a large payload flowing remote -> client at the
/// same time. Each payload is larger than the socket buffers, so if the two
/// directions were serialized this full-duplex exchange would dead-lock once the
/// buffers fill; instead both transfers complete.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relays_both_directions_concurrently() {
    const LEN: usize = 1 << 20; // 1 MiB per direction.
    let to_remote = vec![0xA5u8; LEN];
    let to_client = vec![0x5Au8; LEN];

    // A remote that simultaneously sends `to_client` and drains everything the
    // client sends, returning what it received.
    let listener = TcpListener::bind(LOOPBACK)
        .await
        .expect("failed to bind remote");
    let remote_addr = listener.local_addr().expect("remote local_addr");
    let server_payload = to_client.clone();
    let remote = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("remote accept");
        let (mut read_half, mut write_half) = stream.into_split();
        let send = tokio::spawn(async move {
            write_half
                .write_all(&server_payload)
                .await
                .expect("remote failed to write");
            write_half
                .shutdown()
                .await
                .expect("remote failed to shut down");
        });
        let mut received = Vec::new();
        read_half
            .read_to_end(&mut received)
            .await
            .expect("remote failed to read");
        send.await.expect("remote send task panicked");
        received
    });

    let proxy_addr = spawn_proxy(remote_addr).await;

    let client = connect(proxy_addr).await;
    let (mut client_read, mut client_write) = client.into_split();
    let client_payload = to_remote.clone();
    let client_send = tokio::spawn(async move {
        client_write
            .write_all(&client_payload)
            .await
            .expect("client failed to write");
        client_write
            .shutdown()
            .await
            .expect("client failed to shut down");
    });

    let mut from_remote = Vec::new();
    timeout(IO_TIMEOUT, client_read.read_to_end(&mut from_remote))
        .await
        .expect("client read timed out")
        .expect("client failed to read");
    client_send.await.expect("client send task panicked");

    let from_client = timeout(IO_TIMEOUT, remote)
        .await
        .expect("remote timed out")
        .expect("remote task panicked");

    assert_eq!(
        from_remote.len(),
        LEN,
        "client should receive the full remote payload"
    );
    assert!(
        from_remote == to_client,
        "remote -> client payload corrupted"
    );
    assert_eq!(
        from_client.len(),
        LEN,
        "remote should receive the full client payload"
    );
    assert!(
        from_client == to_remote,
        "client -> remote payload corrupted"
    );
}
