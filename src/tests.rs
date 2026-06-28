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
use crate::args::PayloadFormattingKind;
use crate::args::TimestampPrecision;
use crate::conn::initialize_tcp_listener;
use crate::conn::run_accept_loop;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::time::sleep;
use tokio::time::timeout;
use tokio_modbus::ExceptionCode;
use tokio_modbus::Request as ModbusRequest;
use tokio_modbus::Response as ModbusResponse;
use tokio_modbus::client::Reader;
use tokio_modbus::server::Service as ModbusService;
use tokio_modbus::server::tcp::Server as ModbusServer;
use tokio_modbus::server::tcp::accept_tcp_connection;

/// Upper bound for any single network operation in the tests. Generous enough to
/// avoid flakiness on a loaded CI runner, small enough to fail fast on a hang.
const IO_TIMEOUT: Duration = Duration::from_secs(10);

/// Loopback bind address using an ephemeral port (`127.0.0.1:0`). Always `127.0.0.1`
/// (never `localhost`, which can resolve to IPv6 on Windows).
const LOOPBACK: &str = "127.0.0.1:0";

/// A generous connection cap used by the helpers, so tests that open several
/// concurrent connections are not throttled by the limit under test.
const TEST_MAX_CONNECTIONS: u32 = 512;

/// Build proxy `Arguments` pointing at `remote_addr`, with logging silenced so
/// test output stays clean.
fn test_arguments(
    bind_listener_addr: SocketAddr,
    remote_addr: SocketAddr,
    timeout: Option<u64>,
    max_connections: u32,
) -> Arguments {
    Arguments {
        level: LoggingLevel::Off,
        bind_listener_addr,
        remote_addr,
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

/// Bind a proxy listener on an ephemeral loopback port and start its accept loop
/// in the background. Returns the proxy's bound address. Uses a bounded idle
/// timeout so a stuck test fails fast rather than hanging.
///
/// There is no handle to stop it: the accept loop is intentionally infinite (a
/// proxy keeps listening), and per-connection cleanup is automatic — each
/// connection's relay tasks and sockets are released when it closes. The accept
/// loop itself is cancelled when the test's runtime is dropped at the end of the
/// test, which also releases the ephemeral port.
async fn spawn_proxy(remote_addr: SocketAddr) -> SocketAddr {
    spawn_proxy_full(
        remote_addr,
        Some(IO_TIMEOUT.as_secs()),
        TEST_MAX_CONNECTIONS,
    )
    .await
}

/// Like [`spawn_proxy`] but with an explicit `--timeout` value, where `None` is
/// the default behaviour of no idle timeout.
async fn spawn_proxy_with_timeout(remote_addr: SocketAddr, timeout: Option<u64>) -> SocketAddr {
    spawn_proxy_full(remote_addr, timeout, TEST_MAX_CONNECTIONS).await
}

/// Like [`spawn_proxy`] but with an explicit `--max-connections` cap.
async fn spawn_proxy_with_limit(remote_addr: SocketAddr, max_connections: u32) -> SocketAddr {
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

/// A real Modbus TCP exchange is relayed through the proxy: a `tokio-modbus`
/// client reads holding registers from a real `tokio-modbus` server sitting
/// behind the proxy and receives the expected values. This exercises the proxy
/// with genuine MODBUS framing (its original use case), not just dummy bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relays_a_real_modbus_exchange() {
    const REGISTERS: [u16; 4] = [0x1111, 0x2222, 0x3333, 0x4444];

    // A minimal real Modbus server that serves `REGISTERS` for read-holding-registers.
    struct Service;
    impl ModbusService for Service {
        type Request = ModbusRequest<'static>;
        type Response = ModbusResponse;
        type Exception = ExceptionCode;
        type Future = std::future::Ready<Result<Self::Response, Self::Exception>>;

        fn call(&self, request: Self::Request) -> Self::Future {
            let response = match request {
                ModbusRequest::ReadHoldingRegisters(addr, cnt) => {
                    let start = addr as usize;
                    let end = start + cnt as usize;
                    if end <= REGISTERS.len() {
                        Ok(ModbusResponse::ReadHoldingRegisters(
                            REGISTERS[start..end].to_vec(),
                        ))
                    } else {
                        Err(ExceptionCode::IllegalDataAddress)
                    }
                }
                _ => Err(ExceptionCode::IllegalFunction),
            };
            std::future::ready(response)
        }
    }

    let listener = TcpListener::bind(LOOPBACK)
        .await
        .expect("failed to bind modbus server");
    let modbus_addr = listener.local_addr().expect("modbus local_addr");
    tokio::spawn(async move {
        let server = ModbusServer::new(listener);
        let new_service = |_socket_addr| Ok(Some(Service));
        let on_connected = move |stream, socket_addr| async move {
            accept_tcp_connection(stream, socket_addr, new_service)
        };
        let on_process_error = |err| eprintln!("modbus server error: {err}");
        let _ = server.serve(&on_connected, on_process_error).await;
    });

    let proxy_addr = spawn_proxy(modbus_addr).await;

    let mut ctx = tokio_modbus::client::tcp::connect(proxy_addr)
        .await
        .expect("failed to connect modbus client through proxy");
    let registers = timeout(
        IO_TIMEOUT,
        ctx.read_holding_registers(0, REGISTERS.len() as u16),
    )
    .await
    .expect("modbus read timed out")
    .expect("modbus read failed")
    .expect("modbus returned an exception");

    assert_eq!(
        registers,
        REGISTERS.to_vec(),
        "holding registers must round-trip through the proxy"
    );
}

/// A real HTTP/1.1 exchange is relayed through the proxy: a client request reaches
/// a real `tiny_http` server behind the proxy and the response comes back intact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relays_a_real_http_exchange() {
    const BODY: &str = "Hello through the proxy";

    // A real (blocking) HTTP server; it answers one request then its thread ends.
    let server = tiny_http::Server::http("127.0.0.1:0").expect("failed to start http server");
    let http_addr = server
        .server_addr()
        .to_ip()
        .expect("http server ip address");
    std::thread::spawn(move || {
        if let Ok(request) = server.recv() {
            let _ = request.respond(tiny_http::Response::from_string(BODY));
        }
    });

    let proxy_addr = spawn_proxy(http_addr).await;

    let mut client = connect(proxy_addr).await;
    timeout(
        IO_TIMEOUT,
        client.write_all(b"GET / HTTP/1.1\r\nHost: proxy-test\r\nConnection: close\r\n\r\n"),
    )
    .await
    .expect("http write timed out")
    .expect("failed to send http request");

    // Read until the response body arrives (or the server closes).
    let mut response = Vec::new();
    let mut buffer = [0u8; 1024];
    loop {
        let read_length = timeout(IO_TIMEOUT, client.read(&mut buffer))
            .await
            .expect("http read timed out")
            .expect("failed to read http response");
        if read_length == 0 {
            break;
        }
        response.extend_from_slice(&buffer[0..read_length]);
        if String::from_utf8_lossy(&response).contains(BODY) {
            break;
        }
    }

    let text = String::from_utf8_lossy(&response);
    assert!(
        text.starts_with("HTTP/1.1 200"),
        "expected a 200 response through the proxy, got: {text}"
    );
    assert!(
        text.contains(BODY),
        "the HTTP response body must round-trip through the proxy"
    );
}

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

/// `--timeout` is range-validated by clap: `0` and values large enough to overflow
/// the monotonic clock are rejected, a normal value parses, and omitting it yields
/// `None` (no timeout).
#[test]
fn timeout_argument_is_range_validated() {
    use clap::Parser;

    fn parse(extra: &[&str]) -> Result<Arguments, clap::Error> {
        let mut argv = vec!["logged_tcp_proxy", "-b", "127.0.0.1:0", "-r", "127.0.0.1:0"];
        argv.extend_from_slice(extra);
        Arguments::try_parse_from(argv)
    }

    assert_eq!(
        parse(&[]).expect("omitting --timeout should parse").timeout,
        None,
    );
    assert_eq!(
        parse(&["-t", "30"])
            .expect("a normal --timeout should parse")
            .timeout,
        Some(30),
    );
    assert!(parse(&["-t", "1"]).is_ok(), "the minimum (1) is accepted");
    assert!(
        parse(&["-t", "3153600000"]).is_ok(),
        "the maximum (~100 years) is accepted"
    );
    assert!(parse(&["-t", "0"]).is_err(), "0 is rejected");
    assert!(
        parse(&["-t", "3153600001"]).is_err(),
        "above the maximum is rejected"
    );
    assert!(
        parse(&["-t", "18446744073709551615"]).is_err(),
        "u64::MAX (which would overflow the clock) is rejected"
    );
}

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

/// `--max-connections` has a default and rejects 0 (which would accept nothing).
#[test]
fn max_connections_has_a_default_and_rejects_zero() {
    use clap::Parser;

    fn parse(extra: &[&str]) -> Result<Arguments, clap::Error> {
        let mut argv = vec!["logged_tcp_proxy", "-b", "127.0.0.1:0", "-r", "127.0.0.1:0"];
        argv.extend_from_slice(extra);
        Arguments::try_parse_from(argv)
    }

    assert_eq!(
        parse(&[])
            .expect("default max-connections should parse")
            .max_connections,
        512,
    );
    assert_eq!(
        parse(&["-m", "32"])
            .expect("an explicit max-connections should parse")
            .max_connections,
        32,
    );
    assert!(parse(&["-m", "0"]).is_err(), "0 is rejected");
}

/// The CLI value enums must expose exactly the value names the proxy has always
/// accepted. These names used to come from hand-written `ValueEnum`/`FromStr`/
/// `Display` impls and are now derived (`#[derive(ValueEnum)]` plus the
/// `argument_impl_*` macros). This test pins the derived `to_possible_value()`
/// names — and the `FromStr` → `Display` round-trip — to those exact strings, so a
/// casing change in the derive (e.g. clap's default kebab-case turning `LowerHex`
/// into `lower-hex`) can't silently break `--formatting lowerhex` or any other
/// documented value, or the matching `default_value` on `Arguments`.
#[test]
fn value_enum_names_match_documented_cli_values() {
    use clap::ValueEnum;

    macro_rules! check {
        ($ty:ty, $expected:expr) => {{
            let expected: Vec<String> = $expected.iter().map(|s: &&str| s.to_string()).collect();

            // `to_possible_value()` (now derived) must yield exactly the documented
            // names, in declaration order.
            let names: Vec<String> = <$ty>::value_variants()
                .iter()
                .map(|variant| variant.to_possible_value().unwrap().get_name().to_owned())
                .collect();
            assert_eq!(
                names, expected,
                concat!(
                    stringify!($ty),
                    ": possible-value names drifted from the documented CLI values"
                ),
            );

            // Each documented name must parse back (FromStr) and `Display` must
            // reproduce it unchanged.
            for name in &expected {
                let parsed = name.parse::<$ty>().expect(concat!(
                    stringify!($ty),
                    ": every documented value must parse via FromStr"
                ));
                assert_eq!(
                    parsed.to_string(),
                    *name,
                    concat!(stringify!($ty), ": Display must round-trip the value name"),
                );
            }
        }};
    }

    check!(
        LoggingLevel,
        &["trace", "debug", "info", "warn", "error", "off"]
    );
    check!(
        PayloadFormattingKind,
        &["decimal", "lowerhex", "upperhex", "binary", "octal"]
    );
    check!(
        TimestampPrecision,
        &["seconds", "milliseconds", "microseconds", "nanoseconds"]
    );
}

/// `--threads` has a default and is range-validated: `0` (which Tokio forbids) and
/// values above the cap are rejected, while the bounds and a normal value parse.
#[test]
fn threads_has_a_default_and_is_range_validated() {
    use clap::Parser;

    fn parse(extra: &[&str]) -> Result<Arguments, clap::Error> {
        let mut argv = vec!["logged_tcp_proxy", "-b", "127.0.0.1:0", "-r", "127.0.0.1:0"];
        argv.extend_from_slice(extra);
        Arguments::try_parse_from(argv)
    }

    assert_eq!(parse(&[]).expect("default threads should parse").threads, 4,);
    assert_eq!(
        parse(&["--threads", "16"])
            .expect("an explicit threads should parse")
            .threads,
        16,
    );
    assert_eq!(
        parse(&["-w", "16"])
            .expect("the -w short flag should parse")
            .threads,
        16,
        "-w is the short alias for --threads",
    );
    assert!(
        parse(&["--threads", "1"]).is_ok(),
        "the minimum (1) is accepted"
    );
    assert!(
        parse(&["--threads", "1024"]).is_ok(),
        "the maximum (1024) is accepted"
    );
    assert!(parse(&["--threads", "0"]).is_err(), "0 is rejected");
    assert!(
        parse(&["--threads", "1025"]).is_err(),
        "above the maximum is rejected"
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
