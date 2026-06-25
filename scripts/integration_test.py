#!/usr/bin/env python3
"""Black-box integration test for the ``logged_tcp_proxy`` binary.

Unlike the in-crate tests in ``src/tests.rs`` (which call the relay functions
directly), this script exercises the *real compiled binary* end to end:

  * it starts a tiny echo server,
  * runs the proxy binary between a client and that echo server, and
  * checks that bytes the client sends are relayed to the echo server and back,
    AND that the proxy prints the payload to the console in the requested format
    (the whole point of the tool).

It uses only the Python standard library (sockets + subprocess + threads), so it
runs the same way on Linux, macOS and Windows.

Usage:

    python3 scripts/integration_test.py

By default it builds the debug binary with ``cargo build`` first. To test an
already-built binary (e.g. a release build) without rebuilding, point it at one:

    LOGGED_TCP_PROXY_BIN=target/release/logged_tcp_proxy python3 scripts/integration_test.py

Exits 0 if every case passes, non-zero otherwise.
"""

import http.server
import os
import platform
import signal
import socket
import struct
import subprocess
import sys
import threading
import time
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HOST = "127.0.0.1"
START_TIMEOUT = 15.0  # seconds to wait for the proxy to start listening
IO_TIMEOUT = 5.0      # seconds for any single client socket operation


def fail(message):
    print("FAIL: " + message)
    sys.exit(1)


def binary_path():
    """Return the path to the proxy binary, building it if necessary."""
    override = os.environ.get("LOGGED_TCP_PROXY_BIN")
    if override:
        if not os.path.isfile(override):
            fail("LOGGED_TCP_PROXY_BIN points at a missing file: " + override)
        return override

    print("building binary with `cargo build` ...")
    subprocess.run(["cargo", "build"], cwd=ROOT, check=True)
    name = "logged_tcp_proxy" + (".exe" if platform.system() == "Windows" else "")
    path = os.path.join(ROOT, "target", "debug", name)
    if not os.path.isfile(path):
        fail("binary not found after build: " + path)
    return path


def start_echo_server():
    """Start an echo server on an ephemeral port. Returns (socket, port)."""
    server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind((HOST, 0))
    server.listen(8)
    port = server.getsockname()[1]

    def serve():
        while True:
            try:
                conn, _ = server.accept()
            except OSError:
                return  # server socket closed -> stop
            threading.Thread(target=echo_conn, args=(conn,), daemon=True).start()

    def echo_conn(conn):
        with conn:
            while True:
                try:
                    data = conn.recv(4096)
                except OSError:
                    return
                if not data:
                    return
                conn.sendall(data)

    threading.Thread(target=serve, daemon=True).start()
    return server, port


def free_port():
    """Reserve an ephemeral port, then release it for the proxy to bind."""
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.bind((HOST, 0))
    port = s.getsockname()[1]
    s.close()
    return port


def wait_for_listener(port):
    """Block until something is accepting connections on `port`."""
    deadline = time.monotonic() + START_TIMEOUT
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((HOST, port), timeout=0.5):
                return True
        except OSError:
            time.sleep(0.05)
    return False


def stop_proxy(proxy):
    """Terminate the proxy process and return its captured output."""
    proxy.terminate()
    try:
        output, _ = proxy.communicate(timeout=5)
    except subprocess.TimeoutExpired:
        proxy.kill()
        output, _ = proxy.communicate()
    return output


def recv_exact(sock, count):
    """Read exactly `count` bytes from `sock`, or None on early EOF."""
    chunks = []
    remaining = count
    while remaining > 0:
        chunk = sock.recv(remaining)
        if not chunk:
            return None
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def start_proxy(binary, remote_port, level="debug"):
    """Spawn the proxy on a free port pointing at `remote_port`. Returns (proc, port)."""
    proxy_port = free_port()
    proxy = subprocess.Popen(
        [
            binary,
            "--bind-listener-addr", "%s:%d" % (HOST, proxy_port),
            "--remote-addr", "%s:%d" % (HOST, remote_port),
            "--level", level,
        ],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    return proxy, proxy_port


def run_case(binary, formatting, separator, render_byte):
    """Run one end-to-end case for a given `--formatting`/`--separator`."""
    echo_server, echo_port = start_echo_server()
    proxy_port = free_port()
    payload = bytes([0x00, 0x01, 0x6F, 0x03, 0xFF, 0x10, 0x2A])

    proxy = subprocess.Popen(
        [
            binary,
            "--bind-listener-addr", "%s:%d" % (HOST, proxy_port),
            "--remote-addr", "%s:%d" % (HOST, echo_port),
            "--formatting", formatting,
            "--separator", separator,
            "--level", "debug",
        ],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,  # env_logger writes to stderr; capture both
        text=True,
    )

    try:
        if not wait_for_listener(proxy_port):
            fail("[%s] proxy did not start listening within %ss" % (formatting, START_TIMEOUT))

        with socket.create_connection((HOST, proxy_port), timeout=IO_TIMEOUT) as client:
            client.settimeout(IO_TIMEOUT)
            client.sendall(payload)
            received = b""
            while len(received) < len(payload):
                chunk = client.recv(4096)
                if not chunk:
                    break
                received += chunk

        if received != payload:
            fail("[%s] echo mismatch: sent %r, got %r" % (formatting, payload, received))

        # Give the proxy a moment to flush its log lines before we stop it.
        time.sleep(0.3)
    finally:
        output = stop_proxy(proxy)
        echo_server.close()

    expected = separator.join(render_byte(b) for b in payload)
    if expected not in output:
        print("---- proxy output ----\n" + output + "----------------------")
        fail("[%s] payload not logged as %r" % (formatting, expected))

    print("OK [%s] relayed %d bytes and logged them as %s" % (formatting, len(payload), expected))


def test_unreachable_remote(binary):
    """With the remote down, the proxy must not panic: it logs the failure, closes
    the accepted client cleanly, and keeps serving."""
    remote_port = free_port()  # nothing is listening here
    proxy_port = free_port()
    proxy = subprocess.Popen(
        [
            binary,
            "--bind-listener-addr", "%s:%d" % (HOST, proxy_port),
            "--remote-addr", "%s:%d" % (HOST, remote_port),
            "--level", "debug",
        ],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    try:
        if not wait_for_listener(proxy_port):
            fail("[unreachable-remote] proxy did not start listening")

        with socket.create_connection((HOST, proxy_port), timeout=IO_TIMEOUT) as client:
            client.settimeout(IO_TIMEOUT)
            try:
                leftover = client.recv(16)  # expect a clean close (b"")
            except ConnectionResetError:
                leftover = b""
            if leftover != b"":
                fail("[unreachable-remote] proxy did not close the client, got %r" % leftover)

        time.sleep(0.2)
        if proxy.poll() is not None:
            fail("[unreachable-remote] proxy exited after a failed remote connect (rc=%s)"
                 % proxy.returncode)
    finally:
        output = stop_proxy(proxy)

    if "panic" in output.lower():
        print("---- proxy output ----\n" + output + "----------------------")
        fail("[unreachable-remote] proxy panicked instead of handling the error gracefully")
    print("OK [unreachable-remote] failure logged, client closed, proxy still serving")


def test_bind_failure(binary):
    """Binding to an address already in use must exit non-zero without panicking."""
    occupied = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    occupied.bind((HOST, 0))
    occupied.listen(1)
    in_use_port = occupied.getsockname()[1]
    remote_port = free_port()
    try:
        completed = subprocess.run(
            [
                binary,
                "--bind-listener-addr", "%s:%d" % (HOST, in_use_port),
                "--remote-addr", "%s:%d" % (HOST, remote_port),
                "--level", "debug",
            ],
            cwd=ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            timeout=15,
        )
    finally:
        occupied.close()

    if completed.returncode == 0:
        fail("[bind-failure] expected a non-zero exit when the bind address is in use")
    if "panic" in completed.stdout.lower():
        print("---- proxy output ----\n" + completed.stdout + "----------------------")
        fail("[bind-failure] proxy panicked instead of exiting cleanly")
    print("OK [bind-failure] exited non-zero (rc=%d) with a clean error" % completed.returncode)


def test_ctrl_c(binary):
    """Ctrl-C (SIGINT) triggers a clean shutdown with a zero exit code."""
    if platform.system() == "Windows":
        print("SKIP [ctrl-c] SIGINT delivery is tested only on POSIX")
        return

    echo_server, echo_port = start_echo_server()
    proxy_port = free_port()
    proxy = subprocess.Popen(
        [
            binary,
            "--bind-listener-addr", "%s:%d" % (HOST, proxy_port),
            "--remote-addr", "%s:%d" % (HOST, echo_port),
            "--level", "info",
        ],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    output = ""
    try:
        if not wait_for_listener(proxy_port):
            fail("[ctrl-c] proxy did not start listening")
        proxy.send_signal(signal.SIGINT)
        try:
            output = proxy.communicate(timeout=5)[0]
        except subprocess.TimeoutExpired:
            proxy.kill()
            fail("[ctrl-c] proxy did not exit within 5s of SIGINT")
    finally:
        echo_server.close()
        if proxy.poll() is None:
            proxy.kill()

    if proxy.returncode != 0:
        print("---- proxy output ----\n" + output + "----------------------")
        fail("[ctrl-c] expected a clean exit (0) after SIGINT, got rc=%s" % proxy.returncode)
    print("OK [ctrl-c] proxy shut down cleanly on SIGINT (rc=0)")


def test_http(binary):
    """A real HTTP request/response (stdlib http.server + urllib) is relayed."""
    body = b"Hello through the proxy"

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):  # noqa: N802 (name mandated by BaseHTTPRequestHandler)
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args):
            pass  # keep test output quiet

    httpd = http.server.HTTPServer((HOST, 0), Handler)
    http_port = httpd.server_address[1]
    threading.Thread(target=httpd.serve_forever, daemon=True).start()

    proxy, proxy_port = start_proxy(binary, http_port)
    try:
        if not wait_for_listener(proxy_port):
            fail("[http] proxy did not start listening")
        url = "http://%s:%d/" % (HOST, proxy_port)
        # Talk to the proxy directly, ignoring any HTTP_PROXY in the environment.
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with opener.open(url, timeout=IO_TIMEOUT) as response:
            status = response.status
            received = response.read()
        if status != 200:
            fail("[http] expected status 200, got %s" % status)
        if received != body:
            fail("[http] response body mismatch: %r" % received)
    finally:
        httpd.shutdown()
        output = stop_proxy(proxy)

    if "panic" in output.lower():
        print("---- proxy output ----\n" + output + "----------------------")
        fail("[http] proxy panicked while relaying HTTP")
    print("OK [http] real HTTP request relayed through the proxy")


def start_modbus_server(registers):
    """Start a minimal real Modbus TCP server serving `registers`. Returns (sock, port)."""
    server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind((HOST, 0))
    server.listen(8)
    port = server.getsockname()[1]

    def handle(conn):
        with conn:
            while True:
                header = recv_exact(conn, 7)  # MBAP header
                if header is None:
                    return
                transaction_id, _protocol, length, unit = struct.unpack(">HHHB", header)
                pdu = recv_exact(conn, length - 1)  # length counts the unit id + PDU
                if pdu is None:
                    return
                function = pdu[0]
                if function == 0x03:  # read holding registers
                    start, qty = struct.unpack(">HH", pdu[1:5])
                    data = b"".join(struct.pack(">H", registers[start + i]) for i in range(qty))
                    response_pdu = struct.pack(">BB", 0x03, len(data)) + data
                else:  # illegal-function exception
                    response_pdu = struct.pack(">BB", function | 0x80, 0x01)
                frame = struct.pack(">HHHB", transaction_id, 0, len(response_pdu) + 1, unit)
                conn.sendall(frame + response_pdu)

    def serve():
        while True:
            try:
                conn, _ = server.accept()
            except OSError:
                return
            threading.Thread(target=handle, args=(conn,), daemon=True).start()

    threading.Thread(target=serve, daemon=True).start()
    return server, port


def test_modbus(binary):
    """A real Modbus TCP read-holding-registers exchange is relayed and logged."""
    registers = [0x1111, 0x2222, 0x3333, 0x4444]
    modbus_server, modbus_port = start_modbus_server(registers)
    proxy, proxy_port = start_proxy(binary, modbus_port)

    transaction_id = 0x0001
    request_pdu = struct.pack(">BHH", 0x03, 0x0000, len(registers))  # fc, start addr, count
    request = struct.pack(">HHHB", transaction_id, 0, len(request_pdu) + 1, 0x01) + request_pdu

    try:
        if not wait_for_listener(proxy_port):
            fail("[modbus] proxy did not start listening")
        with socket.create_connection((HOST, proxy_port), timeout=IO_TIMEOUT) as client:
            client.settimeout(IO_TIMEOUT)
            client.sendall(request)
            header = recv_exact(client, 7)
            if header is None:
                fail("[modbus] no response header relayed by the proxy")
            response_tid, _protocol, length, _unit = struct.unpack(">HHHB", header)
            response_pdu = recv_exact(client, length - 1)
            if response_pdu is None:
                fail("[modbus] truncated response relayed by the proxy")
    finally:
        modbus_server.close()
        output = stop_proxy(proxy)

    if response_tid != transaction_id:
        fail("[modbus] transaction id mismatch: %d != %d" % (response_tid, transaction_id))
    if response_pdu[0] != 0x03 or response_pdu[1] != len(registers) * 2:
        fail("[modbus] unexpected response PDU: %r" % response_pdu)
    values = list(struct.unpack(">" + "H" * len(registers), response_pdu[2:2 + response_pdu[1]]))
    if values != registers:
        fail("[modbus] register values mismatch: %r != %r" % (values, registers))

    # The proxy should have logged the raw request frame in hex (its whole purpose).
    request_hex = ":".join("%02x" % b for b in request)
    if request_hex not in output:
        print("---- proxy output ----\n" + output + "----------------------")
        fail("[modbus] proxy did not log the MODBUS request frame %s" % request_hex)
    print("OK [modbus] real MODBUS read-holding-registers relayed and logged")


def test_threads(binary):
    """The runtime honors `--threads`: a custom count still relays bytes, and an
    invalid count (0, which Tokio forbids) is rejected at startup."""
    # A valid custom thread count must produce a working runtime that relays.
    echo_server, echo_port = start_echo_server()
    proxy_port = free_port()
    payload = bytes([0xDE, 0xAD, 0xBE, 0xEF])
    proxy = subprocess.Popen(
        [
            binary,
            "--bind-listener-addr", "%s:%d" % (HOST, proxy_port),
            "--remote-addr", "%s:%d" % (HOST, echo_port),
            "--threads", "8",
            "--level", "info",
        ],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    try:
        if not wait_for_listener(proxy_port):
            fail("[threads] proxy did not start listening with --threads 8")
        with socket.create_connection((HOST, proxy_port), timeout=IO_TIMEOUT) as client:
            client.settimeout(IO_TIMEOUT)
            client.sendall(payload)
            received = recv_exact(client, len(payload))
        if received != payload:
            fail("[threads] echo mismatch: sent %r, got %r" % (payload, received))
    finally:
        output = stop_proxy(proxy)
        echo_server.close()
    if "panic" in output.lower():
        print("---- proxy output ----\n" + output + "----------------------")
        fail("[threads] proxy panicked with a custom thread count")

    # An out-of-range count (0) must be rejected by clap with a non-zero exit.
    completed = subprocess.run(
        [
            binary,
            "--bind-listener-addr", "%s:%d" % (HOST, free_port()),
            "--remote-addr", "%s:%d" % (HOST, free_port()),
            "--threads", "0",
        ],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        timeout=15,
    )
    if completed.returncode == 0:
        fail("[threads] expected a non-zero exit for --threads 0")
    if "panic" in completed.stdout.lower():
        print("---- proxy output ----\n" + completed.stdout + "----------------------")
        fail("[threads] proxy panicked instead of rejecting --threads 0")
    print("OK [threads] custom thread count relays and 0 is rejected")


def main():
    binary = binary_path()
    print("testing binary: " + binary)
    run_case(binary, "lowerhex", ":", lambda b: "%02x" % b)
    run_case(binary, "upperhex", "-", lambda b: "%02X" % b)
    test_http(binary)
    test_modbus(binary)
    test_unreachable_remote(binary)
    test_bind_failure(binary)
    test_threads(binary)
    test_ctrl_c(binary)
    print("integration test passed")


if __name__ == "__main__":
    main()
