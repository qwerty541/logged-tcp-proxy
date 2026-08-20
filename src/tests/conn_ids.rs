//! The per-connection id tags on console output: ids are sequential in accept
//! order, one connection's lifecycle lines share one tag, and
//! `--no-connection-ids` disables the tags.
//!
//! The payload (`<`/`>`) lines log at `debug`, which the shared capture
//! deliberately filters out (see `log_capture`), so their tags are pinned by the
//! black-box `scripts/integration_test.py` instead; these tests cover the tagged
//! `info`/`error` lifecycle lines. Ids restart at 1 for every proxy, and the
//! whole suite shares one capture buffer, so every assertion keys off this
//! test's own unique ephemeral addresses.

use super::helpers::IO_TIMEOUT;
use super::helpers::LOOPBACK;
use super::helpers::TEST_MAX_CONNECTIONS;
use super::helpers::assert_round_trip;
use super::helpers::connect;
use super::helpers::spawn_echo_server;
use super::helpers::spawn_localhost_echo_server;
use super::helpers::spawn_proxy;
use super::helpers::spawn_proxy_configured;
use super::helpers::spawn_proxy_with_target;
use super::helpers::spawn_proxy_with_timeout;
use super::log_capture::captured_lines;
use super::log_capture::install_capturing_logger;
use crate::args::TargetAddr;
use std::net::SocketAddr;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::time::timeout;

/// Like `spawn_proxy`, but with `--no-connection-ids` set. Local to this module —
/// only the opt-out test needs it.
async fn spawn_proxy_without_ids(remote_addr: SocketAddr) -> SocketAddr {
    spawn_proxy_configured(
        remote_addr,
        Some(IO_TIMEOUT.as_secs()),
        TEST_MAX_CONNECTIONS,
        |arguments| arguments.connection_ids = false,
    )
    .await
}

/// Ids are minted sequentially in accept order, starting at 1 for each proxy run,
/// and the accept line carries the tag. The first round trip completes before the
/// second client connects, so the accept order — and therefore the ids — is
/// deterministic.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connections_get_distinct_sequential_id_tags() {
    install_capturing_logger();

    let echo_addr = spawn_echo_server().await;
    let proxy_addr = spawn_proxy(echo_addr).await;

    let mut first = connect(proxy_addr).await;
    assert_round_trip(&mut first, b"first tagged connection").await;
    let first_addr = first.local_addr().expect("first client local_addr");

    let mut second = connect(proxy_addr).await;
    assert_round_trip(&mut second, b"second tagged connection").await;
    let second_addr = second.local_addr().expect("second client local_addr");

    let lines = captured_lines();
    assert!(
        lines.contains(&format!("[#1] Incoming connection from {first_addr}")),
        "the first connection's accept line must be tagged [#1]; captured: {lines:?}"
    );
    assert!(
        lines.contains(&format!("[#2] Incoming connection from {second_addr}")),
        "the second connection's accept line must be tagged [#2]; captured: {lines:?}"
    );
}

/// A connection's lifecycle lines share the accept line's tag: the id is learned
/// from this test's own accept line (keyed by the client's unique ephemeral
/// address) and the `Connected to destination` line must carry the same tag.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lifecycle_lines_share_the_accept_line_tag() {
    install_capturing_logger();

    let named_port = spawn_localhost_echo_server().await;
    let proxy_addr = spawn_proxy_with_target(TargetAddr::Named {
        host: "localhost".to_string(),
        port: named_port,
    })
    .await;

    let mut client = connect(proxy_addr).await;
    assert_round_trip(&mut client, b"tagged lifecycle").await;
    let client_addr = client.local_addr().expect("client local_addr");

    let lines = captured_lines();
    let accept_suffix = format!("Incoming connection from {client_addr}");
    let tag = lines
        .iter()
        .find_map(|line| line.strip_suffix(accept_suffix.as_str()))
        .expect("the accept line for this connection must be captured")
        .to_string();
    assert!(
        tag.starts_with("[#") && tag.ends_with("] "),
        "the accept line must start with a `[#N] ` tag, got {tag:?}"
    );
    // Token-exact target comparison (not `starts_with`): a prefix match on an
    // ephemeral port could be satisfied by a parallel test's `Connected` line
    // whose port merely extends this one (`:4523` vs `:45231`) — the same hazard
    // `logged_destinations` documents.
    let expected_target = format!("localhost:{named_port}");
    assert!(
        lines.iter().any(|line| {
            line.strip_prefix(tag.as_str())
                .and_then(|message| message.strip_prefix("Connected to destination "))
                .and_then(|rest| rest.split_whitespace().next())
                == Some(expected_target.as_str())
        }),
        "the Connected line must carry the same {tag:?} tag and name {expected_target}; captured: {lines:?}"
    );
}

/// The destination connect-failure line is tagged. Waiting for the client-side
/// close guarantees the handler logged the failure before the capture is read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_failure_line_carries_the_connection_tag() {
    install_capturing_logger();

    // Reserve a port, then release it so nothing is listening there.
    let dead = TcpListener::bind(LOOPBACK)
        .await
        .expect("failed to bind to reserve a dead port");
    let dead_remote_addr = dead.local_addr().expect("dead local_addr");
    drop(dead);

    let proxy_addr = spawn_proxy(dead_remote_addr).await;
    let mut client = connect(proxy_addr).await;
    let mut buffer = [0u8; 16];
    let _ = timeout(IO_TIMEOUT, client.read(&mut buffer))
        .await
        .expect("client read timed out: the proxy did not close after the failed connect");

    let lines = captured_lines();
    // This proxy's first connection is #1; the dead port keys the line to this test.
    let expected_start = format!("[#1] Failed to connect to destination {dead_remote_addr}:");
    assert!(
        lines.iter().any(|line| line.starts_with(&expected_start)),
        "the connect-failure line must be tagged [#1]; captured: {lines:?}"
    );
}

/// The idle-close line is tagged and names the client it closed, so the line is
/// unique to this test even though other tests also trigger idle closes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_close_line_carries_the_connection_tag() {
    install_capturing_logger();

    let echo_addr = spawn_echo_server().await;
    let proxy_addr = spawn_proxy_with_timeout(echo_addr, Some(1)).await;

    let mut client = connect(proxy_addr).await;
    let client_addr = client.local_addr().expect("client local_addr");
    // Stay idle until the watchdog closes the connection. The proxy logs the
    // idle-close line *before* the `select!` drops the relays (which is what
    // closes this socket), so once the close is observed here the capture is
    // guaranteed to hold the line.
    let mut buffer = [0u8; 16];
    let result = timeout(IO_TIMEOUT, client.read(&mut buffer))
        .await
        .expect("client read timed out: the idle timeout did not fire");
    match result {
        Ok(0) | Err(_) => {} // clean end-of-stream or a reset: the connection closed
        Ok(n) => panic!("expected the idle connection to close, but read {n} bytes"),
    }

    let lines = captured_lines();
    let expected =
        format!("[#1] Closing idle connection from {client_addr} after 1s of inactivity");
    assert!(
        lines.contains(&expected),
        "the idle-close line must be tagged and name the client; captured: {lines:?}"
    );
}

/// With `--no-connection-ids` the tags disappear: the accept line is captured in
/// its exact untagged form (an equality match, so a tagged line cannot pass).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_connection_ids_disables_the_tags() {
    install_capturing_logger();

    let echo_addr = spawn_echo_server().await;
    let proxy_addr = spawn_proxy_without_ids(echo_addr).await;

    let mut client = connect(proxy_addr).await;
    assert_round_trip(&mut client, b"untagged connection").await;
    let client_addr = client.local_addr().expect("client local_addr");

    let lines = captured_lines();
    assert!(
        lines.contains(&format!("Incoming connection from {client_addr}")),
        "with ids disabled the accept line must be exactly the untagged form; captured: {lines:?}"
    );
}
