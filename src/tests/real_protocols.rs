//! Realism tests: genuine MODBUS TCP and HTTP/1.1 conversations are relayed
//! unchanged. The proxy never parses either protocol; these pin its actual
//! use case rather than adding code coverage.

use super::helpers::IO_TIMEOUT;
use super::helpers::LOOPBACK;
use super::helpers::connect;
use super::helpers::spawn_proxy;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_modbus::ExceptionCode;
use tokio_modbus::Request as ModbusRequest;
use tokio_modbus::Response as ModbusResponse;
use tokio_modbus::client::Reader;
use tokio_modbus::server::Service as ModbusService;
use tokio_modbus::server::tcp::Server as ModbusServer;
use tokio_modbus::server::tcp::accept_tcp_connection;

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
