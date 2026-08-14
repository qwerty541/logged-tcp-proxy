//! Connection teardown: a close on either side is forwarded to the other, and a
//! client half-close still lets the remote's response through.

use super::helpers::IO_TIMEOUT;
use super::helpers::LOOPBACK;
use super::helpers::connect;
use super::helpers::spawn_proxy;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::time::timeout;

/// When the client closes its connection, the proxy forwards that close to the
/// remote instead of holding the remote half open (a regression guard for the
/// end-of-stream handling in the relay).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_client_propagates_to_remote() {
    // A remote that accepts one connection, drains until end-of-stream, then
    // returns. The task completing is the observable signal that the proxy
    // forwarded the client's close.
    let listener = TcpListener::bind(LOOPBACK)
        .await
        .expect("failed to bind remote");
    let remote_addr = listener.local_addr().expect("remote local_addr");
    let remote = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("remote accept");
        let mut buffer = [0u8; 4096];
        loop {
            match stream.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    });

    let proxy_addr = spawn_proxy(remote_addr).await;

    let mut client = connect(proxy_addr).await;
    timeout(IO_TIMEOUT, client.write_all(b"ping"))
        .await
        .expect("write timed out")
        .expect("failed to write to proxy");
    timeout(IO_TIMEOUT, client.shutdown())
        .await
        .expect("shutdown timed out")
        .expect("failed to close client");

    timeout(IO_TIMEOUT, remote)
        .await
        .expect("remote still open: proxy did not forward the client close")
        .expect("remote task panicked");
}

/// When the remote closes its connection, the proxy forwards that close to the
/// client: the client's read returns end-of-stream instead of hanging forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_remote_propagates_to_client() {
    // A remote that closes immediately after accepting the proxy's connection.
    let listener = TcpListener::bind(LOOPBACK)
        .await
        .expect("failed to bind remote");
    let remote_addr = listener.local_addr().expect("remote local_addr");
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("remote accept");
        drop(stream);
    });

    let proxy_addr = spawn_proxy(remote_addr).await;

    let mut client = connect(proxy_addr).await;
    let mut buffer = [0u8; 16];
    let read_length = timeout(IO_TIMEOUT, client.read(&mut buffer))
        .await
        .expect("client read timed out: proxy did not forward the remote close")
        .expect("client read errored");
    assert_eq!(
        read_length, 0,
        "expected end-of-stream after the remote closed"
    );
}

/// A client may finish sending (half-close its write side) while still waiting for
/// the remote's response. The proxy must keep relaying the remote -> client
/// direction until the remote is done, rather than tearing the whole connection
/// down when the client's send side ends — which would truncate the response.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn half_closed_client_still_receives_response() {
    const RESPONSE: &[u8] = b"RESPONSE-AFTER-HALF-CLOSE";

    // A remote that reads the request until end-of-stream (the client's half-close,
    // forwarded by the proxy), then sends its response and closes.
    let listener = TcpListener::bind(LOOPBACK)
        .await
        .expect("failed to bind remote");
    let remote_addr = listener.local_addr().expect("remote local_addr");
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("remote accept");
        let mut scratch = [0u8; 1024];
        loop {
            match stream.read(&mut scratch).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        stream
            .write_all(RESPONSE)
            .await
            .expect("remote failed to write response");
        stream.shutdown().await.expect("remote failed to shut down");
    });

    let proxy_addr = spawn_proxy(remote_addr).await;

    let mut client = connect(proxy_addr).await;
    timeout(IO_TIMEOUT, client.write_all(b"REQUEST"))
        .await
        .expect("write timed out")
        .expect("failed to write request");
    // Finish sending, but keep the read side open for the response.
    timeout(IO_TIMEOUT, client.shutdown())
        .await
        .expect("shutdown timed out")
        .expect("failed to half-close client");

    let mut response = Vec::new();
    timeout(IO_TIMEOUT, client.read_to_end(&mut response))
        .await
        .expect("read timed out")
        .expect("failed to read response");

    assert_eq!(
        response, RESPONSE,
        "the full remote response must arrive after a client half-close"
    );
}
