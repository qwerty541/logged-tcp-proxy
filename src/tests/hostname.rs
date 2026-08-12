//! A `hostname:port` remote: it is resolved via DNS at connect time and relayed
//! like any other target, and only such a target logs the destination it reached.

use super::helpers::assert_round_trip;
use super::helpers::connect;
use super::helpers::spawn_echo_server;
use super::helpers::spawn_localhost_echo_server;
use super::helpers::spawn_proxy;
use super::helpers::spawn_proxy_with_target;
use super::log_capture::install_capturing_logger;
use super::log_capture::logged_destinations;
use crate::args::TargetAddr;

/// A `hostname:port` remote is resolved via DNS at connect time and relayed like
/// any other target. The echo server binds via the same name the proxy resolves
/// (`localhost`) and its assigned port is read back, so the address family always
/// matches on platforms where `localhost` is IPv6 (e.g. Windows); tokio's connect
/// also tries every resolved address, so v4/v6 ordering never matters.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relays_through_a_dns_resolved_hostname() {
    let echo_port = spawn_localhost_echo_server().await;

    let proxy_addr = spawn_proxy_with_target(TargetAddr::Named {
        host: "localhost".to_string(),
        port: echo_port,
    })
    .await;

    let mut client = connect(proxy_addr).await;
    assert_round_trip(&mut client, b"resolved through DNS").await;
}

/// Only a *hostname* target reports the destination it reached. A literal `IP:port`
/// remote must stay silent, since `Connected to destination 127.0.0.1:x
/// (127.0.0.1:x)` would merely repeat itself — the `TargetAddr::Named` guard in
/// `incoming_connection_handle` is the only thing enforcing that, and nothing else
/// in either test layer would notice if it were removed.
///
/// Both halves are asserted in one test on purpose: the "must not log" half would
/// pass vacuously if the capture were broken, so it is paired with a "must log"
/// half that fails first in that case.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_a_hostname_remote_logs_the_resolved_destination() {
    install_capturing_logger();

    // A literal `IP:port` remote — the `TargetAddr::Socket` variant.
    let echo_addr = spawn_echo_server().await;
    let literal_proxy_addr = spawn_proxy(echo_addr).await;
    let mut literal_client = connect(literal_proxy_addr).await;
    assert_round_trip(&mut literal_client, b"literal address").await;

    // A `hostname:port` remote — the `TargetAddr::Named` variant.
    let named_port = spawn_localhost_echo_server().await;
    let named_proxy_addr = spawn_proxy_with_target(TargetAddr::Named {
        host: "localhost".to_string(),
        port: named_port,
    })
    .await;
    let mut named_client = connect(named_proxy_addr).await;
    assert_round_trip(&mut named_client, b"resolved name").await;

    // Both round trips completed, so each connection had already reached its
    // destination and logged whatever it was going to log.
    let destinations = logged_destinations();
    assert!(
        destinations.contains(&format!("localhost:{named_port}")),
        "a hostname remote must report the destination it reached; captured: {destinations:?}"
    );
    assert!(
        !destinations.contains(&echo_addr.to_string()),
        "a literal IP remote must not log a redundant destination line; captured: {destinations:?}"
    );
}
