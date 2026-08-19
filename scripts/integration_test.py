#!/usr/bin/env python3
"""Black-box integration test for the ``logged_tcp_proxy`` binary.

Unlike the in-crate tests in ``src/tests/`` (which call the relay functions
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
import re
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


def _serve_echo(server):
    """Run the accept + echo loop for an already-bound, listening server socket."""

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

    def serve():
        while True:
            try:
                conn, _ = server.accept()
            except OSError:
                return  # server socket closed -> stop
            threading.Thread(target=echo_conn, args=(conn,), daemon=True).start()

    threading.Thread(target=serve, daemon=True).start()


def start_echo_server():
    """Start an echo server on an ephemeral 127.0.0.1 port. Returns (socket, port)."""
    server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind((HOST, 0))
    server.listen(8)
    port = server.getsockname()[1]
    _serve_echo(server)
    return server, port


def start_echo_server_on(host):
    """Start an echo server bound to whatever address family `host` resolves to (via
    getaddrinfo — the same name the proxy resolves), so the server is actually
    listening on an address the proxy will reach, even where `host` (e.g. `localhost`)
    resolves only to IPv6. Returns (socket, port)."""
    family, socktype, proto, _canon, sockaddr = socket.getaddrinfo(
        host, 0, type=socket.SOCK_STREAM
    )[0]
    server = socket.socket(family, socktype, proto)
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(sockaddr)
    server.listen(8)
    port = server.getsockname()[1]
    _serve_echo(server)
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


def start_output_drain(proxy):
    """Continuously read the proxy's stdout into a list of lines from a background
    thread, so a test can poll the output while the proxy is still running. Once
    started, the thread owns the pipe: collect the final output with
    `drain_proxy_output()` instead of `stop_proxy()`. Returns (thread, lines)."""
    lines = []

    def _drain():
        for line in proxy.stdout:
            lines.append(line)

    thread = threading.Thread(target=_drain, daemon=True)
    thread.start()
    return thread, lines


def drain_proxy_output(proxy, thread, lines):
    """Terminate the proxy and return everything the drain thread captured."""
    proxy.terminate()
    try:
        proxy.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proxy.kill()
        proxy.wait()
    thread.join(timeout=5)
    return "".join(lines)


def assert_tagged_connect_failure(output, case):
    """Assert the tagged connect-failure line was logged. Presence, not a count:
    the listener-readiness probe produces its own tagged failure line too."""
    if not re.search(r"\[#\d+\] Failed to connect to destination", output):
        print("---- proxy output ----\n" + output + "----------------------")
        fail("[%s] the tagged connect-failure line was not logged" % case)


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


def start_proxy(binary, remote_port, level="debug", extra_args=()):
    """Spawn the proxy on a free port pointing at `remote_port`. Returns (proc, port)."""
    proxy_port = free_port()
    proxy = subprocess.Popen(
        [
            binary,
            "--bind-listener-addr", "%s:%d" % (HOST, proxy_port),
            "--remote-addr", "%s:%d" % (HOST, remote_port),
            "--level", level,
        ]
        + list(extra_args),
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


def start_asymmetric_server(reply):
    """Start a server that answers every connection with `reply`, whatever it was
    sent. Unlike the echo server the two directions carry DIFFERENT bytes, which is
    what makes it possible to tell which direction a logged line belongs to.
    Returns (socket, port)."""
    server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind((HOST, 0))
    server.listen(8)
    port = server.getsockname()[1]

    def handle(conn):
        with conn:
            try:
                if conn.recv(4096):
                    conn.sendall(reply)
            except OSError:
                return

    def serve():
        while True:
            try:
                conn, _ = server.accept()
            except OSError:
                return
            threading.Thread(target=handle, args=(conn,), daemon=True).start()

    threading.Thread(target=serve, daemon=True).start()
    return server, port


def test_direction_markers_and_no_double_logging(binary):
    """Each direction is marked correctly and logged exactly once.

    `<` marks bytes read from the client, `>` marks bytes written back to it. Both
    directions are logged on the SOURCE stream; the destination stream deliberately
    uses a RecordKindFilter so the same bytes are not printed a second time. An echo
    server cannot check any of this — with identical bytes both ways, swapped markers
    and doubled lines both look correct — so this case uses a remote whose reply
    differs from the request. Both payloads contain hex letters on purpose: a
    digit-only rendering (e.g. `11:22:33`) can collide with the RFC 3339 timestamp
    env_logger stamps on every line, which would break the exactly-once count."""
    request = bytes([0x1A, 0x2B, 0x3C])
    reply = bytes([0xAA, 0xBB, 0xCC])
    request_hex = ":".join("%02x" % b for b in request)
    reply_hex = ":".join("%02x" % b for b in reply)

    remote, remote_port = start_asymmetric_server(reply)
    proxy, proxy_port = start_proxy(binary, remote_port)
    try:
        if not wait_for_listener(proxy_port):
            fail("[markers] proxy did not start listening")
        with socket.create_connection((HOST, proxy_port), timeout=IO_TIMEOUT) as client:
            client.settimeout(IO_TIMEOUT)
            client.sendall(request)
            received = recv_exact(client, len(reply))
        if received != reply:
            fail("[markers] expected the remote's reply %r, got %r" % (reply, received))
        time.sleep(0.3)  # let the proxy flush its log lines
    finally:
        output = stop_proxy(proxy)
        remote.close()

    lines = output.splitlines()
    sent_out = [l for l in lines if ("< " + request_hex) in l]
    reply_in = [l for l in lines if ("> " + reply_hex) in l]

    # Direction: the client's bytes are `<`, the remote's reply is `>`.
    if len(sent_out) != 1 or len(reply_in) != 1:
        print("---- proxy output ----\n" + output + "----------------------")
        fail("[markers] expected exactly one `< %s` and one `> %s` line, got %d and %d"
             % (request_hex, reply_hex, len(sent_out), len(reply_in)))

    # Markers must not be swapped.
    if ("> " + request_hex) in output or ("< " + reply_hex) in output:
        print("---- proxy output ----\n" + output + "----------------------")
        fail("[markers] direction markers are swapped")

    # Both directions belong to one connection, so both payload lines must carry
    # the SAME `[#N]` id tag. The id is extracted rather than assumed: the
    # listener-readiness probe above is itself an accepted connection, so the data
    # connection's id is never simply #1.
    tagged_request = re.search(r"\[#(\d+)\] < " + re.escape(request_hex), output)
    if tagged_request is None:
        print("---- proxy output ----\n" + output + "----------------------")
        fail("[markers] the `<` payload line lacks its `[#N]` connection-id tag")
    conn_id = tagged_request.group(1)
    if ("[#%s] > %s" % (conn_id, reply_hex)) not in output:
        print("---- proxy output ----\n" + output + "----------------------")
        fail("[markers] the `>` reply line does not carry the same `[#%s]` tag" % conn_id)

    # De-duplication: neither payload may appear more than once anywhere in the
    # output, which is what the destination stream's RecordKindFilter guarantees.
    for label, payload_hex in (("request", request_hex), ("reply", reply_hex)):
        occurrences = output.count(payload_hex)
        if occurrences != 1:
            print("---- proxy output ----\n" + output + "----------------------")
            fail("[markers] the %s payload was logged %d times, expected exactly once"
                 % (label, occurrences))

    print("OK [markers] `<`/`>` mark the right direction and each payload is logged once")


def test_connection_id_tags(binary):
    """Concurrent connections get distinct `[#N]` ids, each bound to its own lines.

    Two clients are held open at the same time, so this covers exactly the
    interleaved output the ids exist to disentangle. Ids are extracted by regex and
    correlated through the tagged accept lines and each client's own local port —
    never assumed to be literal #1/#2, because the listener-readiness probe also
    consumes an id."""
    echo_server, echo_port = start_echo_server()
    proxy, proxy_port = start_proxy(binary, echo_port)
    # The drain thread owns the proxy's stdout from the start, so the test can
    # poll the output while the proxy is still running (see the Drop-record wait
    # below) — a fixed post-close sleep would race stop_proxy's SIGTERM, which
    # discards records the proxy has not yet written.
    drain, log_lines = start_output_drain(proxy)
    # Letter-bearing bytes, so the hex renderings can never collide with an
    # RFC 3339 timestamp (see test_direction_markers_and_no_double_logging).
    payload_a = bytes([0x0A, 0x1B, 0x2C])
    payload_b = bytes([0xD3, 0xE4, 0xF5])
    hex_a = ":".join("%02x" % b for b in payload_a)
    hex_b = ":".join("%02x" % b for b in payload_b)

    def id_for(text, port, label):
        found = re.search(
            r"\[#(\d+)\] Incoming connection from %s:%d\b" % (re.escape(HOST), port),
            text,
        )
        if found is None:
            print("---- proxy output ----\n" + text + "----------------------")
            fail("[conn-ids] no tagged accept line for the %s client" % label)
        return found.group(1)

    try:
        if not wait_for_listener(proxy_port):
            fail("[conn-ids] proxy did not start listening")
        with socket.create_connection((HOST, proxy_port), timeout=IO_TIMEOUT) as client_a, \
                socket.create_connection((HOST, proxy_port), timeout=IO_TIMEOUT) as client_b:
            client_a.settimeout(IO_TIMEOUT)
            client_b.settimeout(IO_TIMEOUT)
            port_a = client_a.getsockname()[1]
            port_b = client_b.getsockname()[1]
            # Both clients are open before either payload is sent: the connections
            # genuinely overlap.
            client_a.sendall(payload_a)
            client_b.sendall(payload_b)
            if recv_exact(client_a, len(payload_a)) != payload_a:
                fail("[conn-ids] echo mismatch on the first client")
            if recv_exact(client_b, len(payload_b)) != payload_b:
                fail("[conn-ids] echo mismatch on the second client")
        # The accept lines were logged before the round trips completed, so the
        # ids are already extractable while the proxy still runs.
        id_a = id_for("".join(log_lines), port_a, "first")
        id_b = id_for("".join(log_lines), port_b, "second")
        # Wait (bounded) for both connections' Drop records — emitted only after
        # each handler observes both EOFs and drops its two LoggedStreams — before
        # terminating the proxy.
        deadline = time.time() + IO_TIMEOUT
        while time.time() < deadline:
            text = "".join(log_lines)
            if all(text.count("[#%s] x Deallocated." % i) >= 2 for i in (id_a, id_b)):
                break
            time.sleep(0.05)
    finally:
        output = drain_proxy_output(proxy, drain, log_lines)
        echo_server.close()

    if id_a == id_b:
        print("---- proxy output ----\n" + output + "----------------------")
        fail("[conn-ids] both connections got the same id #%s" % id_a)

    # Each client's payload line carries its own connection's tag.
    for conn_id, payload_hex, label in ((id_a, hex_a, "first"), (id_b, hex_b, "second")):
        if ("[#%s] < %s" % (conn_id, payload_hex)) not in output:
            print("---- proxy output ----\n" + output + "----------------------")
            fail("[conn-ids] the %s client's payload line is not tagged [#%s]"
                 % (label, conn_id))

    # Exactly TWO tagged Drop records per connection — one from the source stream
    # (DefaultFilter logs Drop too) and one from the destination stream. The exact
    # count is what pins the DESTINATION logger's prefix: if it lost the tag, only
    # one tagged record per connection would remain (presence alone would pass
    # vacuously via the source stream's record).
    for conn_id, label in ((id_a, "first"), (id_b, "second")):
        drops = output.count("[#%s] x Deallocated." % conn_id)
        if drops != 2:
            print("---- proxy output ----\n" + output + "----------------------")
            fail("[conn-ids] expected exactly 2 tagged Drop records for the %s "
                 "connection (#%s), got %d" % (label, conn_id, drops))

    print("OK [conn-ids] concurrent connections carry distinct ids #%s and #%s"
          % (id_a, id_b))


def test_no_connection_ids_flag(binary):
    """`--no-connection-ids` disables the tags entirely.

    The output returns to the untagged shape: no `[#` appears anywhere (the
    env_logger `[ts LEVEL]` framing never contains that sequence), while the
    payload and lifecycle lines still print."""
    echo_server, echo_port = start_echo_server()
    proxy, proxy_port = start_proxy(binary, echo_port, extra_args=("--no-connection-ids",))
    payload = bytes([0x5A, 0x6B, 0x7C])
    payload_hex = ":".join("%02x" % b for b in payload)
    try:
        if not wait_for_listener(proxy_port):
            fail("[no-conn-ids] proxy did not start listening")
        with socket.create_connection((HOST, proxy_port), timeout=IO_TIMEOUT) as client:
            client.settimeout(IO_TIMEOUT)
            client.sendall(payload)
            if recv_exact(client, len(payload)) != payload:
                fail("[no-conn-ids] echo mismatch")
        # No flush wait needed: the asserted payload and accept lines are logged
        # before the client can receive its echo, and the `[#` check is an absence.
    finally:
        output = stop_proxy(proxy)
        echo_server.close()

    if payload_hex not in output:
        print("---- proxy output ----\n" + output + "----------------------")
        fail("[no-conn-ids] the payload must still be logged without connection ids")
    if "Incoming connection from" not in output:
        print("---- proxy output ----\n" + output + "----------------------")
        fail("[no-conn-ids] the accept line must still be logged without connection ids")
    if "[#" in output:
        print("---- proxy output ----\n" + output + "----------------------")
        fail("[no-conn-ids] output must carry no `[#N]` tags with --no-connection-ids")

    print("OK [no-conn-ids] --no-connection-ids removes the tags, output otherwise intact")


def test_level_filters_payload(binary):
    """`--level` controls whether the payload is printed at all.

    The payload is logged at `debug`, so it must appear at `--level debug` (the
    default) and be suppressed at `--level info` — while the `INFO` lifecycle lines
    still show, proving the relay really ran and the absence is not vacuous."""
    payload = bytes([0xDE, 0xAD, 0xBE, 0xEF])
    payload_hex = ":".join("%02x" % b for b in payload)

    def relay_at(level):
        echo_server, echo_port = start_echo_server()
        proxy, proxy_port = start_proxy(binary, echo_port, level=level)
        try:
            if not wait_for_listener(proxy_port):
                fail("[level] proxy did not start listening at --level %s" % level)
            with socket.create_connection((HOST, proxy_port), timeout=IO_TIMEOUT) as client:
                client.settimeout(IO_TIMEOUT)
                client.sendall(payload)
                received = recv_exact(client, len(payload))
            if received != payload:
                fail("[level] relay failed at --level %s: got %r" % (level, received))
            time.sleep(0.3)
        finally:
            output = stop_proxy(proxy)
            echo_server.close()
        return output

    debug_output = relay_at("debug")
    if payload_hex not in debug_output:
        print("---- proxy output ----\n" + debug_output + "----------------------")
        fail("[level] the payload must be printed at --level debug")

    info_output = relay_at("info")
    if payload_hex in info_output:
        print("---- proxy output ----\n" + info_output + "----------------------")
        fail("[level] the payload must be hidden at --level info")
    # The relay still ran, so the lifecycle lines prove the absence above is real.
    if "Listener bound to" not in info_output:
        print("---- proxy output ----\n" + info_output + "----------------------")
        fail("[level] --level info should still print the INFO lifecycle lines")
    # The tagged accept line is INFO too: the id->peer mapping must survive
    # `--level info`, where the tagged payload lines are suppressed.
    if not re.search(r"\[#\d+\] Incoming connection from", info_output):
        print("---- proxy output ----\n" + info_output + "----------------------")
        fail("[level] the tagged accept line must remain visible at --level info")

    print("OK [level] payload shown at debug, hidden at info (lifecycle lines kept)")


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
    assert_tagged_connect_failure(output, "unreachable-remote")
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


def test_hostname_remote(binary):
    """A `hostname:port` remote (not just IP:port) is resolved via DNS and relayed.
    The echo server binds on whatever address family `localhost` resolves to (the same
    name the proxy resolves), so it is reachable even on hosts where `localhost` is
    IPv6-only; the proxy also tries every resolved address, so v4/v6 ordering never
    matters."""
    echo_server, echo_port = start_echo_server_on("localhost")
    proxy_port = free_port()
    payload = bytes([0x00, 0x11, 0x22, 0x33, 0x44])
    proxy = subprocess.Popen(
        [
            binary,
            "--bind-listener-addr", "%s:%d" % (HOST, proxy_port),
            "--remote-addr", "localhost:%d" % echo_port,
            "--level", "debug",
        ],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    try:
        if not wait_for_listener(proxy_port):
            fail("[hostname] proxy did not start listening with a hostname remote")
        with socket.create_connection((HOST, proxy_port), timeout=IO_TIMEOUT) as client:
            client.settimeout(IO_TIMEOUT)
            client.sendall(payload)
            received = recv_exact(client, len(payload))
        if received != payload:
            fail("[hostname] echo mismatch through a hostname remote: sent %r, got %r"
                 % (payload, received))
        time.sleep(0.3)
    finally:
        output = stop_proxy(proxy)
        echo_server.close()

    if "panic" in output.lower():
        print("---- proxy output ----\n" + output + "----------------------")
        fail("[hostname] proxy panicked relaying through a hostname remote")
    # The payload must still be logged (lowerhex default), proving the relay ran.
    expected = ":".join("%02x" % b for b in payload)
    if expected not in output:
        print("---- proxy output ----\n" + output + "----------------------")
        fail("[hostname] payload not logged through a hostname remote")
    # And the resolved-peer INFO line names the hostname target it reached, tagged
    # with the connection's id.
    if not re.search(r"\[#\d+\] Connected to destination localhost:%d\b" % echo_port, output):
        print("---- proxy output ----\n" + output + "----------------------")
        fail("[hostname] proxy did not log the tagged resolved destination for a hostname remote")
    print("OK [hostname] relayed through localhost:%d (DNS-resolved) and logged it" % echo_port)


def test_unresolvable_remote(binary):
    """A hostname that never resolves is handled like an unreachable remote: the
    proxy logs the failure, closes the client cleanly, keeps serving, and does not
    panic. `.invalid` is reserved (RFC 6761) and never resolves on any platform, so
    this needs no network."""
    proxy_port = free_port()
    proxy = subprocess.Popen(
        [
            binary,
            "--bind-listener-addr", "%s:%d" % (HOST, proxy_port),
            "--remote-addr", "nonexistent.invalid:65000",
            "--level", "debug",
        ],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    try:
        if not wait_for_listener(proxy_port):
            fail("[unresolvable-remote] proxy did not start listening")

        with socket.create_connection((HOST, proxy_port), timeout=IO_TIMEOUT) as client:
            client.settimeout(IO_TIMEOUT)
            try:
                leftover = client.recv(16)  # expect a clean close (b"")
            except ConnectionResetError:
                leftover = b""
            if leftover != b"":
                fail("[unresolvable-remote] proxy did not close the client, got %r" % leftover)

        time.sleep(0.2)
        if proxy.poll() is not None:
            fail("[unresolvable-remote] proxy exited after a failed resolution (rc=%s)"
                 % proxy.returncode)
    finally:
        output = stop_proxy(proxy)

    if "panic" in output.lower():
        print("---- proxy output ----\n" + output + "----------------------")
        fail("[unresolvable-remote] proxy panicked instead of handling the DNS failure")
    assert_tagged_connect_failure(output, "unresolvable-remote")
    print("OK [unresolvable-remote] DNS failure logged, client closed, proxy still serving")


def main():
    binary = binary_path()
    print("testing binary: " + binary)
    # One case per `--formatting` value: printing the payload in the requested
    # notation is the whole point of the tool, so every mode is exercised against
    # the real binary (the in-crate `tests::formatting` module pins the renderings).
    run_case(binary, "lowerhex", ":", lambda b: "%02x" % b)
    run_case(binary, "upperhex", "-", lambda b: "%02X" % b)
    run_case(binary, "decimal", ":", lambda b: "%d" % b)
    run_case(binary, "octal", ":", lambda b: "%03o" % b)
    run_case(binary, "binary", ":", lambda b: format(b, "08b"))
    test_direction_markers_and_no_double_logging(binary)
    test_connection_id_tags(binary)
    test_no_connection_ids_flag(binary)
    test_level_filters_payload(binary)
    test_hostname_remote(binary)
    test_http(binary)
    test_modbus(binary)
    test_unreachable_remote(binary)
    test_unresolvable_remote(binary)
    test_bind_failure(binary)
    test_threads(binary)
    test_ctrl_c(binary)
    print("integration test passed")


if __name__ == "__main__":
    main()
