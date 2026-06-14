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

import os
import platform
import signal
import socket
import subprocess
import sys
import threading
import time

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


def main():
    binary = binary_path()
    print("testing binary: " + binary)
    run_case(binary, "lowerhex", ":", lambda b: "%02x" % b)
    run_case(binary, "upperhex", "-", lambda b: "%02X" % b)
    test_unreachable_remote(binary)
    test_bind_failure(binary)
    test_ctrl_c(binary)
    print("integration test passed")


if __name__ == "__main__":
    main()
