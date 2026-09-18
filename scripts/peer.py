#!/usr/bin/env python3
"""Scriptable TCP peers for manually testing ``logged_tcp_proxy``.

The proxy is a man-in-the-middle, so watching it work needs something on both
sides of it. This script is both of those things:

  * ``peer.py server``  - the destination the proxy forwards to;
  * ``peer.py client``  - connects to the proxy and talks through it;
  * ``peer.py recipes`` - ready-made three-terminal scenarios to copy-paste.

A typical session uses three terminals. The default addresses line up, so no
flags are needed:

    T1  cargo run -- -b 127.0.0.1:20502 -r 127.0.0.1:20582 -p milliseconds
    T2  python3 scripts/peer.py server
    T3  python3 scripts/peer.py client        # type lines; /help for more

Both peers log what they do in the proxy's own shape, so the terminals can be
read side by side: an RFC 3339 timestamp, then `<` for bytes travelling
client -> server and `>` for server -> client (the same meaning in all three
terminals), rendered with the same -f/-s options as the proxy. The peers also
log what the proxy cannot show: `-` FIN sent, `=` EOF received, `x` socket
closed (and how), `!` socket error, `*` everything else.

Standard library only (Python 3.8+). The black-box test
(scripts/integration_test.py) runs the deterministic recipes against the real
binary in CI, so the recipes at the bottom of this file double as regression
tests.
"""

import argparse
import os
import queue
import re
import select
import shlex
import socket
import struct
import sys
import threading
import time

if sys.version_info < (3, 8):
    sys.exit("peer.py needs Python 3.8 or newer")

# The client connects to the proxy's --bind-listener-addr; the server listens on
# the proxy's --remote-addr. These match the README's example ports.
PROXY_ADDR = "127.0.0.1:20502"
SERVER_ADDR = "127.0.0.1:20582"

# Byte renderings, one per proxy --formatting value, matching logged-stream's
# formatters (so the same bytes print identically in every terminal).
FORMATS = {
    "lowerhex": lambda b: "%02x" % b,
    "upperhex": lambda b: "%02X" % b,
    "decimal": lambda b: "%d" % b,
    "octal": lambda b: "%03o" % b,
    "binary": lambda b: "%08b" % b,
}
# Fractional-second digits for each proxy --precision value.
PRECISIONS = {"seconds": 0, "milliseconds": 3, "microseconds": 6, "nanoseconds": 9}
PREVIEW_LIMIT = 64  # bytes shown in the quoted text preview after the payload
POLL = 0.2  # seconds; every wait polls at this rate so Ctrl-C stays responsive

STEPS_HELP = r"""steps (client arguments, server --on-accept/--on-eof, /STEP in the interactive client):
  TEXT       send text; escapes \n \r \t \0 \\ \xNN (no newline is added)
  t:TEXT     send text that would otherwise read as a step (t:read, t:http://x)
  x:HEX      send raw bytes: x:00:01:6f:ff (':', ' ', '-' and ',' are ignored)
  sleep:S    pause S seconds (fractions allowed)
  read       wait for the next chunk from the other side
  read:N     wait until at least N more bytes have arrived
  shut       half-close: send FIN, keep reading
  hold       send nothing more; wait until the other side closes
  close      graceful end: FIN, wait for the other side's FIN, then close
             (what happens anyway after the last step)
  abort      close now without waiting; data still arriving makes the kernel
             answer with RST
  rst        reset: SO_LINGER 0, then close - always sends RST
  loop       repeat all steps; must be last and needs a sleep: or read step.
             Stops once the other side has closed."""


# --- Output --------------------------------------------------------------------

_output_lock = threading.Lock()


def emit(line):
    """Print one whole line and flush it, so lines from the reader thread and
    the main thread never interleave and a pipe sees each line immediately."""
    with _output_lock:
        sys.stdout.write(line + "\n")
        sys.stdout.flush()


def timestamp(digits):
    """The current UTC time as env_logger prints it: RFC 3339 with a `Z`."""
    ns = time.time_ns()
    seconds, fraction = divmod(ns, 1_000_000_000)
    text = time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(seconds))
    if digits:
        text += "." + ("%09d" % fraction)[:digits]
    return text + "Z"


def preview(data):
    """Quote `data` as printable ASCII (escaping everything else), truncated to
    PREVIEW_LIMIT bytes. ASCII only, so it prints on any console encoding."""
    special = {0x0A: "\\n", 0x0D: "\\r", 0x09: "\\t", 0x5C: "\\\\", 0x22: '\\"'}
    text = "".join(
        special.get(b) or (chr(b) if 0x20 <= b < 0x7F else "\\x%02x" % b)
        for b in data[:PREVIEW_LIMIT]
    )
    more = len(data) - PREVIEW_LIMIT
    return '"%s"' % text + ("...(+%d B)" % more if more > 0 else "")


class Log:
    """One peer's event log: `[<timestamp> <role>] [<conn tag>] <kind> <text>`."""

    def __init__(self, role, opts):
        self.role = role
        self.render_byte = FORMATS[opts.formatting]
        self.separator = opts.separator
        self.digits = PRECISIONS[opts.precision]
        self.show_text = not opts.no_text

    def event(self, tag, kind, text):
        head = "[%s %s]" % (timestamp(self.digits), self.role)
        emit(" ".join(part for part in (head, tag and "[%s]" % tag, kind, text) if part))

    def data(self, tag, arrow, data):
        text = self.separator.join(self.render_byte(b) for b in data)
        if self.show_text:
            text += "  " + preview(data)
        self.event(tag, arrow, text)


def describe(error):
    """`ConnectionResetError: Connection reset by peer`: the exception class
    names the failure the same way on every OS, the text is the OS's own."""
    return "%s: %s" % (type(error).__name__, error.strerror or error)


# --- Steps -----------------------------------------------------------------------


class StepError(ValueError):
    """A step (or address) that does not parse; reported as a usage error."""


ESCAPES = {"n": b"\n", "r": b"\r", "t": b"\t", "0": b"\0", "\\": b"\\"}
KEYWORDS = ("read", "shut", "hold", "close", "abort", "rst", "loop")
TERMINAL = ("close", "abort", "rst")


def unescape(text):
    """Text to UTF-8 bytes, honouring the \\n \\r \\t \\0 \\\\ and \\xNN escapes."""
    out = bytearray()
    i = 0
    while i < len(text):
        char = text[i]
        if char != "\\":
            out += char.encode("utf-8")
            i += 1
            continue
        code = text[i + 1:i + 2]
        if code in ESCAPES:
            out += ESCAPES[code]
            i += 2
        elif code == "x" and re.fullmatch(r"[0-9a-fA-F]{2}", text[i + 2:i + 4]):
            out.append(int(text[i + 2:i + 4], 16))
            i += 4
        else:
            raise StepError("bad escape in %r (use \\n \\r \\t \\0 \\\\ or \\xNN)" % text)
    return bytes(out)


def parse_step(token):
    """One step token -> (kind, argument); see STEPS_HELP."""
    if token in KEYWORDS:
        return (token, None)
    prefixed = re.match(r"([a-z]+):(.*)\Z", token, re.S)
    if not prefixed:
        if not token:
            raise StepError("empty step")
        return ("send", unescape(token))
    name, value = prefixed.groups()
    if name == "t":
        if not value:
            raise StepError("empty step %r" % token)
        return ("send", unescape(value))
    if name == "x":
        try:
            data = bytes.fromhex(re.sub(r"[:\s,-]", "", value))
        except ValueError:
            raise StepError("bad hex in %r" % token) from None
        if not data:
            raise StepError("empty step %r" % token)
        return ("send", data)
    if name == "sleep":
        try:
            seconds = float(value)
        except ValueError:
            seconds = -1.0
        if not seconds >= 0:  # also rejects nan
            raise StepError("bad duration in %r (seconds, e.g. sleep:0.5)" % token)
        return ("sleep", seconds)
    if name == "read":
        if not value.isdigit() or int(value) == 0:
            raise StepError("bad byte count in %r (e.g. read:16)" % token)
        return ("read", int(value))
    raise StepError(
        "unknown step %r; to send it as text write t:%s" % (name + ":", token)
    )


def parse_steps(tokens):
    """Parse a whole step list and check where `loop` and the ending steps sit."""
    steps = [parse_step(token) for token in tokens]
    for i, (kind, _) in enumerate(steps):
        if (kind in TERMINAL or kind == "loop") and i != len(steps) - 1:
            raise StepError("%r must be the last step" % kind)
    if steps and steps[-1][0] == "loop":
        if not any(kind in ("sleep", "read") for kind, _ in steps):
            raise StepError("loop needs a sleep:S or read step to pace it")
    return steps


def parse_addr(text):
    """`host:port`, `[v6]:port` -> (host, port)."""
    host, _, port = text.rpartition(":")
    host = host[1:-1] if host.startswith("[") and host.endswith("]") else host
    if not host or not port.isdigit() or int(port) > 65535:
        raise StepError("bad address %r (expected host:port)" % text)
    return host, int(port)


def show_addr(addr):
    host, port = addr[0], addr[1]
    return ("[%s]:%d" if ":" in host else "%s:%d") % (host, port)


# --- One connection ------------------------------------------------------------------


class Broken(Exception):
    """A send or shutdown failed: the connection is unusable."""


class Conn:
    """A connected socket plus a reader thread that logs everything received.

    Received chunks also go onto a queue, consumed by `read` steps (and by the
    server's reply loop); `None` on the queue marks the end of the stream."""

    def __init__(self, sock, log, tag, role, opts):
        self.sock = sock
        self.log = log
        self.tag = tag
        # The proxy's arrows: `<` is client -> server bytes, `>` server -> client.
        self.out_arrow, self.in_arrow = ("<", ">") if role == "client" else (">", "<")
        self.split = opts.split
        self.gap = opts.gap
        self.sent = 0
        self.received = 0
        self.peer_eof = False  # the other side sent FIN (a clean end)
        self.peer_error = False  # receiving failed (e.g. the other side reset)
        self.shut_sent = False
        self.broken = False
        self.chunks = queue.Queue()
        self.finished = threading.Event()  # the reader has stopped
        self.stop = threading.Event()
        # Our write boundaries should be the user's, not Nagle's. (macOS refuses
        # socket options on a connection that was already reset; that is fine.)
        try:
            sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        except OSError:
            pass
        self.reader = threading.Thread(target=self._read_loop, daemon=True)
        self.reader.start()

    def _read_loop(self):
        try:
            while not self.stop.is_set():
                # Poll instead of blocking in recv(): closing a socket while
                # another thread is blocked in recv() on it can delay the real
                # close - and with it the FIN or RST - until that recv() returns.
                try:
                    ready, _, _ = select.select([self.sock], [], [], 0.1)
                    data = self.sock.recv(65536) if ready else None
                except OSError as error:
                    if not self.stop.is_set():
                        self.peer_error = True
                        self.log.event(self.tag, "!", "receive failed: " + describe(error))
                    return
                if data is None:
                    continue
                if not data:
                    self.peer_eof = True
                    self.log.event(self.tag, "=", "EOF: the other side sent FIN")
                    return
                self.received += len(data)
                self.log.data(self.tag, self.in_arrow, data)
                self.chunks.put(data)
        finally:
            self.chunks.put(None)
            self.finished.set()

    def send(self, data):
        pieces = [data]
        if self.split:
            pieces = [data[i:i + self.split] for i in range(0, len(data), self.split)]
        for i, piece in enumerate(pieces):
            if i and self.gap:
                time.sleep(self.gap)
            # Logged before sending, so the timestamps order as the bytes move:
            # client `<`, then the proxy's `<`, then the server's `<`.
            self.log.data(self.tag, self.out_arrow, piece)
            try:
                self.sock.sendall(piece)
            except OSError as error:
                self.log.event(self.tag, "!", "send failed: " + describe(error))
                self.broken = True
                raise Broken() from None
            self.sent += len(piece)

    def shut(self):
        if self.shut_sent:
            return
        self.log.event(self.tag, "-", "FIN sent (write side shut down)")
        try:
            self.sock.shutdown(socket.SHUT_WR)
        except OSError as error:
            self.log.event(self.tag, "!", "shutdown failed: " + describe(error))
            self.broken = True
            raise Broken() from None
        self.shut_sent = True

    def next_chunk(self):
        """The next received chunk not yet consumed, waiting for it; None once
        the other side has finished (FIN or error)."""
        while True:
            try:
                chunk = self.chunks.get(timeout=POLL)
            except queue.Empty:
                continue
            if chunk is None:
                self.chunks.put(None)  # keep the end marker for later callers
            return chunk

    def read(self, count):
        """Wait for one chunk, or for `count` bytes. False if the stream ended first."""
        got = 0
        while got < (count or 1):
            chunk = self.next_chunk()
            if chunk is None:
                return False
            got += len(chunk)
        return True

    def hold(self):
        while not self.finished.wait(POLL):
            pass

    def finish(self, how):
        """End the connection: `close` (graceful), `abort`, `rst`, or `interrupted`.
        A graceful close of a connection that already failed just closes it."""
        if how == "close" and not (self.broken or self.peer_error):
            try:
                self.shut()
            except Broken:
                pass
            if not self.finished.wait(1.0):
                self.log.event(self.tag, "*", "waiting for the other side to close (Ctrl-C quits)")
                self.hold()
        # Stop the reader before closing (see _read_loop).
        self.stop.set()
        self.reader.join()
        if how == "rst":
            # l_onoff=1, l_linger=0: close() discards unsent data and sends RST.
            # Windows' struct linger is two u_shorts, everywhere else two ints.
            linger = struct.pack("HH" if os.name == "nt" else "ii", 1, 0)
            try:
                self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, linger)
            except OSError as error:
                self.log.event(self.tag, "!", "cannot arm the RST: " + describe(error))
        detail = {
            "close": "closed",
            "abort": "closed without waiting (abort)",
            "rst": "reset (RST sent)",
            "interrupted": "closed (interrupted)",
        }[how]
        self.log.event(
            self.tag, "x", "%s: sent %d B, received %d B" % (detail, self.sent, self.received)
        )
        self.sock.close()


def run_steps(conn, steps):
    """Run `steps`; return the ending step that ran (`close`/`abort`/`rst`), or
    None. Raises Broken if the connection failed underneath."""
    looping = bool(steps) and steps[-1][0] == "loop"
    while True:
        for kind, arg in steps:
            if kind == "send":
                conn.send(arg)
            elif kind == "sleep":
                time.sleep(arg)
            elif kind == "read":
                if not conn.read(arg) and looping:
                    return None
            elif kind == "shut":
                conn.shut()
            elif kind == "hold":
                conn.hold()
            elif kind in TERMINAL:
                return kind
            elif kind == "loop" and conn.finished.is_set():
                return None
        if not looping:
            return None


# --- server ----------------------------------------------------------------------------


def handle(sock, addr, log, opts):
    """Serve one accepted connection: --on-accept steps, the reply loop, then
    --on-eof steps once the other side has sent FIN."""
    conn = Conn(sock, log, "s:%d" % addr[1], "server", opts)
    log.event(conn.tag, "*", "accepted from %s" % show_addr(addr))
    ending = None
    try:
        ending = run_steps(conn, opts.on_accept)
        looped = bool(opts.on_accept) and opts.on_accept[-1][0] == "loop"
        # A looping --on-accept script replaces the reply loop.
        if ending is None and not looped:
            while True:
                chunk = conn.next_chunk()
                if chunk is None:
                    break
                if opts.mode == "echo":
                    conn.send(chunk)
                elif opts.mode == "prefix":
                    conn.send(opts.prefix + chunk)
        if ending is None and conn.peer_eof:
            ending = run_steps(conn, opts.on_eof)
    except Broken:
        pass
    conn.finish(ending or "close")


def run_server(opts, log):
    host, port = parse_addr(opts.listen)
    family, _, _, _, sockaddr = socket.getaddrinfo(host, port, type=socket.SOCK_STREAM)[0]
    listener = socket.create_server(sockaddr[:2], family=family)
    # A timeout keeps accept() interruptible by Ctrl-C (on Windows too).
    listener.settimeout(POLL)
    mode = {
        "prefix": "mode prefix: replies %s + data" % preview(opts.prefix),
        "echo": "mode echo: replies the data",
        "sink": "mode sink: never replies",
    }[opts.mode]
    log.event(None, "*", "listening on %s (%s)" % (show_addr(listener.getsockname()), mode))
    try:
        while True:
            try:
                sock, addr = listener.accept()
            except socket.timeout:
                continue
            sock.setblocking(True)
            if opts.once:
                listener.close()
                handle(sock, addr, log, opts)
                return 0
            threading.Thread(target=handle, args=(sock, addr, log, opts), daemon=True).start()
    except KeyboardInterrupt:
        log.event(None, "*", "stopped")
        return 0
    finally:
        listener.close()


# --- client ----------------------------------------------------------------------------


def connect(opts, log):
    """Connect to the proxy, retrying only while the connection is refused.

    Nothing is listening yet when a connection is refused, so waiting this way
    never opens a probe connection: the client's first real connection is the
    proxy's `[#1]`."""
    host, port = parse_addr(opts.connect)
    deadline = time.monotonic() + opts.retry
    announced = False
    while True:
        try:
            return socket.create_connection((host, port))
        except ConnectionRefusedError as error:
            if time.monotonic() >= deadline:
                log.event(None, "!", "connect to %s failed: %s" % (opts.connect, describe(error)))
                return None
            if not announced:
                log.event(None, "*", "%s refused the connection; retrying for up to %gs"
                          % (opts.connect, opts.retry))
                announced = True
            time.sleep(POLL)
        except OSError as error:
            log.event(None, "!", "connect to %s failed: %s" % (opts.connect, describe(error)))
            return None


def repl(conn, log, eol):
    """Interactive mode: each typed line is sent as-is plus `eol`; `/STEP` runs
    a step. End of input (Ctrl-D) ends the connection gracefully."""
    if sys.stdin.isatty():
        try:
            import readline  # noqa: F401 (line editing and history where available)
        except ImportError:
            pass
    log.event(None, "*", "type a line to send it; /help lists the steps; "
                         "Ctrl-D ends gracefully, Ctrl-C quits")
    while True:
        try:
            line = input()
        except EOFError:
            return "close"
        if not line.startswith("/") or line.startswith("//"):
            text = line[1:] if line.startswith("//") else line
            conn.send(text.encode("utf-8") + eol)
            continue
        if line == "/help":
            emit("  (a plain line is sent as-is plus the --eol ending; //text sends /text)")
            emit(STEPS_HELP)
            continue
        try:
            step = parse_step(line[1:])
        except StepError as error:
            log.event(None, "!", "%s (/help lists the steps)" % error)
            continue
        if step[0] == "loop":
            log.event(None, "!", "loop only works in a step list")
            continue
        ending = run_steps(conn, [step])
        if ending:
            return ending


def run_client(opts, log):
    sock = connect(opts, log)
    if sock is None:
        return 1
    local, remote = sock.getsockname(), sock.getpeername()
    conn = Conn(sock, log, "c:%d" % local[1], "client", opts)
    log.event(conn.tag, "*", "connected to %s from %s (the proxy logs it as "
                             "'Incoming connection from %s')"
              % (show_addr(remote), show_addr(local), show_addr(local)))
    eol = {"lf": b"\n", "crlf": b"\r\n", "none": b""}[opts.eol]
    ending = "close"
    try:
        if opts.steps:
            ending = run_steps(conn, opts.steps) or "close"
        else:
            ending = repl(conn, log, eol)
    except Broken:
        ending = "close"
    except KeyboardInterrupt:
        ending = "interrupted"
    try:
        conn.finish(ending)
    except KeyboardInterrupt:  # Ctrl-C while waiting for the other side to close
        conn.finish("interrupted")
    return 0


# --- recipes ---------------------------------------------------------------------------

# Ready-made scenarios: what to run in each terminal and what to look for.
# `proxy` is appended to PROXY_BASE and `remote` overrides the proxy's -r;
# `server: None` means leave the destination down. A recipe with `ci: True`
# is run by scripts/integration_test.py against the real binary (with the
# addresses swapped for ephemeral ports): `expect` lists substrings each
# terminal must print, in order, and `absent` substrings it must not print.
# Keep `ci` for scenarios whose output is deterministic on every OS.
PROXY_BASE = ["-p", "milliseconds"]

RECIPES = [
    {
        "name": "explore",
        "about": "Talk through the proxy interactively.",
        "proxy": [],
        "server": [],
        "client": [],
        "look": [
            "T3: type lines; /x:00 01 6f ff sends raw bytes; /help lists the steps.",
            "The same bytes carry the same arrow in all three terminals: < is",
            "client -> server, > is server -> client. The server replies 're: ' + data,",
            "so the two directions differ. Ctrl-D in T3 ends the conversation gracefully.",
        ],
    },
    {
        "name": "directions",
        "about": "One request, one reply: `<` and `>` in the proxy's log.",
        "proxy": [],
        "server": [],
        "client": ["hello\\n", "read"],
        "look": [
            "T1: [#1] < 68:65:6c:6c:6f:0a (hello) then [#1] > 72:65:3a:20:... (re: hello).",
            "Then two '- Writer shutdown request.' lines (one per direction, as each",
            "FIN is forwarded) and two 'x Deallocated.' lines.",
        ],
        "ci": True,
        "expect": {
            "client": ["< 68:65:6c:6c:6f:0a", "> 72:65:3a:20:68:65:6c:6c:6f:0a",
                       "- FIN sent", "= EOF", "x closed"],
            "proxy": ["[#1] < 68:65:6c:6c:6f:0a", "[#1] > 72:65:3a:20:68:65:6c:6c:6f:0a",
                      "[#1] - Writer shutdown request.", "[#1] x Deallocated."],
            "server": ["< 68:65:6c:6c:6f:0a", "> 72:65:3a:20:68:65:6c:6c:6f:0a",
                       "= EOF", "- FIN sent", "x closed"],
        },
    },
    {
        "name": "formats",
        "about": "Binary payload rendered as decimal in every terminal.",
        "proxy": ["-f", "decimal"],
        "server": ["-f", "decimal", "--mode", "echo"],
        "client": ["-f", "decimal", "x:00:01:6f:ff", "read"],
        "look": [
            "All three terminals print 0:1:111:255 for the bytes 00 01 6f ff.",
            "Try the other -f values (lowerhex, upperhex, octal, binary) and -s, e.g.",
            "-s ' ', in all three terminals.",
        ],
        "ci": True,
        "expect": {
            "client": ["< 0:1:111:255", "> 0:1:111:255"],
            "proxy": ["[#1] < 0:1:111:255", "[#1] > 0:1:111:255"],
            "server": ["< 0:1:111:255", "> 0:1:111:255"],
        },
    },
    {
        "name": "pipe",
        "about": "Pipe lines into the client, like `nc -N`.",
        "proxy": [],
        "server": ["--mode", "echo"],
        "client": [],
        "stdin": "hello\n/x:00 01 6f ff\n",
        "look": [
            "Each input line is sent with a \\n; /STEP lines run steps. End of input",
            "closes gracefully once the echo has come back. The proxy logs one line",
            "per read, so the two sends may show up as one line.",
        ],
        "ci": True,
        "expect": {
            "client": ["< 68:65:6c:6c:6f:0a", "- FIN sent", "= EOF", "x closed"],
            "proxy": ["[#1] < 68:65:6c:6c:6f:0a", "[#1] x Deallocated."],
            "server": ["< 68:65:6c:6c:6f:0a", "= EOF", "x closed"],
        },
    },
    {
        "name": "banner",
        "about": "The server speaks first (SMTP/FTP style).",
        "proxy": [],
        "server": ["--on-accept", "220 ready\\r\\n"],
        "client": ["read", "HELO me\\r\\n", "read"],
        "look": [
            "T1: the first payload line is > 32:32:30:... (the banner), before any <.",
        ],
        "ci": True,
        "expect": {
            "client": ["> 32:32:30:20:72:65:61:64:79:0d:0a", "< 48:45:4c:4f:20:6d:65:0d:0a"],
            "proxy": ["[#1] > 32:32:30:20:72:65:61:64:79:0d:0a",
                      "[#1] < 48:45:4c:4f:20:6d:65:0d:0a"],
            "server": ["> 32:32:30:20:72:65:61:64:79:0d:0a", "< 48:45:4c:4f:20:6d:65:0d:0a"],
        },
    },
    {
        "name": "drip",
        "about": "One byte per write: log lines are reads, not messages.",
        "proxy": [],
        "server": ["--mode", "sink"],
        "client": ["--split", "1", "--gap", "0.2", "hello\\n"],
        "look": [
            "T1: one [#1] < line per byte, about 200 ms apart. The opposite also",
            "happens: writes that arrive together are read, and logged, as one line.",
        ],
    },
    {
        "name": "ticker",
        "about": "Server push only; that one direction keeps the idle timeout away.",
        "proxy": ["-t", "3"],
        "server": ["--mode", "sink", "--on-accept", "tick\\n", "sleep:1", "loop"],
        "client": ["hold"],
        "look": [
            "T1: a [#1] > line every second and no idle close, although the client",
            "never sends. Stop the client with Ctrl-C; the server then stops ticking.",
        ],
    },
    {
        "name": "half-close",
        "about": "The client half-closes; the reply still arrives afterwards.",
        "proxy": [],
        "server": ["--mode", "sink", "--on-eof", "sleep:0.5", "RESPONSE\\n"],
        "client": ["REQ\\n", "shut"],
        "look": [
            "T1: < REQ, then '- Writer shutdown request.' at once (the client's FIN,",
            "forwarded), then 0.5 s later > RESPONSE: data still flows the other way.",
            "T3 prints '- FIN sent' before it receives the response.",
        ],
        "ci": True,
        "expect": {
            "client": ["< 52:45:51:0a", "- FIN sent", "> 52:45:53:50:4f:4e:53:45:0a", "= EOF"],
            "proxy": ["[#1] < 52:45:51:0a", "[#1] - Writer shutdown request.",
                      "[#1] > 52:45:53:50:4f:4e:53:45:0a"],
            "server": ["< 52:45:51:0a", "= EOF", "> 52:45:53:50:4f:4e:53:45:0a",
                       "- FIN sent", "x closed"],
        },
    },
    {
        "name": "server-half-close",
        "about": "The server half-closes first; the client may still send.",
        "proxy": [],
        "server": ["--mode", "sink", "--on-accept", "BYE\\n", "shut"],
        "client": ["read", "sleep:0.3", "LATE\\n"],
        "look": [
            "T1: > BYE, '- Writer shutdown request.', then < LATE: the server still",
            "receives data after its own FIN (T2 logs it).",
        ],
        "ci": True,
        "expect": {
            "client": ["> 42:59:45:0a", "= EOF", "< 4c:41:54:45:0a", "- FIN sent"],
            "proxy": ["[#1] > 42:59:45:0a", "[#1] - Writer shutdown request.",
                      "[#1] < 4c:41:54:45:0a"],
            "server": ["> 42:59:45:0a", "- FIN sent", "< 4c:41:54:45:0a", "= EOF"],
        },
    },
    {
        "name": "server-abort",
        "about": "The server closes after one read; the client keeps sending.",
        "proxy": [],
        "server": ["--mode", "sink", "--on-accept", "read", "abort"],
        "client": ["one", "sleep:0.5", "two", "sleep:0.5", "three"],
        "look": [
            "T1: < one, the server's FIN forwarded, < two (logged, but the closed",
            "server answers it with RST), < three, then an ERROR line",
            "'! Error during async write'. The proxy logs bytes it could not deliver.",
            "T3 only sees a clean EOF.",
        ],
    },
    {
        "name": "client-rst",
        "about": "The client resets; the server sees an ordinary FIN.",
        "proxy": [],
        "server": [],
        "client": ["hello\\n", "read", "rst"],
        "look": [
            "T1: an ERROR line '! Error during async read: ...reset...'. T2 logs a",
            "plain '= EOF': the proxy turns the RST into a FIN towards the server.",
            "Rerun the proxy with -l info: payload lines vanish, the ERROR line stays.",
        ],
        "ci": True,
        "expect": {
            "client": ["< 68:65:6c:6c:6f:0a", "> 72:65:3a:20", "x reset (RST sent)"],
            "proxy": ["[#1] < 68:65:6c:6c:6f:0a", "[#1] ! Error during async read"],
            "server": ["< 68:65:6c:6c:6f:0a", "= EOF", "x closed"],
        },
        "absent": {"server": [" ! "]},
    },
    {
        "name": "server-rst",
        "about": "The server resets; the client sees an ordinary FIN.",
        "proxy": [],
        "server": ["--on-accept", "read", "rst"],
        "client": ["hello\\n", "read"],
        "look": [
            "T1: '! Error during async read: ...reset...'. T3 logs a plain '= EOF':",
            "RSTs do not cross the proxy.",
        ],
        "ci": True,
        "expect": {
            "client": ["< 68:65:6c:6c:6f:0a", "= EOF", "x closed"],
            "proxy": ["[#1] < 68:65:6c:6c:6f:0a", "[#1] ! Error during async read"],
            "server": ["< 68:65:6c:6c:6f:0a", "x reset (RST sent)"],
        },
        "absent": {"client": [" ! "]},
    },
    {
        "name": "idle",
        "about": "--timeout closes a connection that went quiet.",
        "proxy": ["-t", "2"],
        "server": [],
        "client": ["hello\\n", "read", "hold"],
        "look": [
            "T1: 2 s after the last payload line, '[#1] Closing idle connection from",
            "127.0.0.1:<port> after 2s of inactivity'. Both peers log '= EOF'.",
        ],
        "ci": True,
        "expect": {
            "client": ["> 72:65:3a:20", "= EOF", "x closed"],
            "proxy": ["[#1] Closing idle connection from 127.0.0.1:", "[#1] x Deallocated."],
            "server": ["= EOF", "x closed"],
        },
    },
    {
        "name": "max-connections",
        "about": "With -m 1, a second client waits for a free slot.",
        "proxy": ["-m", "1"],
        "server": [],
        "client": ["hello\\n", "read", "hold"],
        "look": [
            "Run the client command in a fourth terminal too. It connects and sends",
            "at once, but the proxy logs no [#2] line and relays nothing until the",
            "first client quits (Ctrl-C in T3).",
        ],
    },
    {
        "name": "dest-down",
        "about": "Nothing listens at the destination.",
        "proxy": [],
        "server": None,
        "client": ["hello\\n", "read"],
        "look": [
            "T1: '[#1] Failed to connect to destination ...' at ERROR level. T3 had",
            "already sent data, so it usually sees a reset rather than a clean EOF.",
        ],
    },
    {
        "name": "ctrl-c",
        "about": "Stopping the proxy with live connections.",
        "proxy": [],
        "server": [],
        "client": ["hello\\n", "read", "hold"],
        "look": [
            "Press Ctrl-C in T1: 'Received shutdown signal, stopping listener.', then",
            "the connection's 'x Deallocated.' lines; both peers log '= EOF'.",
            "(Closing the terminal or `kill` sends SIGTERM instead, which skips all that.)",
        ],
    },
    {
        "name": "modbus",
        "about": "A MODBUS/TCP master polling a canned device every second.",
        "proxy": [],
        "server": ["--mode", "sink", "--on-accept", "read",
                   "x:00:01:00:00:00:07:01:03:04:00:0a:00:0b", "loop"],
        "client": ["x:00:01:00:00:00:06:01:03:00:00:00:02", "read", "sleep:1", "loop"],
        "look": [
            "T1: each second, < the request (read 2 holding registers from unit 1)",
            "and > the response (values 10 and 11). Stop with Ctrl-C in T3.",
        ],
    },
]


def recipe_commands(recipe):
    """The three terminals' commands for `recipe`, as argv lists (None = unused)."""
    remote = recipe.get("remote", SERVER_ADDR)
    proxy = ["cargo", "run", "--", "-b", PROXY_ADDR, "-r", remote] + PROXY_BASE + recipe["proxy"]
    peer = ["python3", "scripts/peer.py"]
    server = None if recipe["server"] is None else peer + ["server"] + recipe["server"]
    return proxy, server, peer + ["client"] + recipe["client"]


def show_recipes(name):
    if name is None:
        width = max(len(recipe["name"]) for recipe in RECIPES)
        for recipe in RECIPES:
            emit("%-*s  %s" % (width, recipe["name"], recipe["about"]))
        emit("\nShow one with: python3 scripts/peer.py recipes NAME")
        return 0
    recipe = next((r for r in RECIPES if r["name"] == name), None)
    if recipe is None:
        emit("no recipe %r; run `python3 scripts/peer.py recipes` for the list" % name)
        return 2
    proxy, server, client = recipe_commands(recipe)
    if "stdin" in recipe:
        client_line = "printf %s | %s" % (shlex.quote(recipe["stdin"].replace("\n", "\\n")),
                                          shlex.join(client))
    else:
        client_line = shlex.join(client)
    emit("%s: %s\n" % (recipe["name"], recipe["about"]))
    emit("  T1 proxy   " + shlex.join(proxy))
    emit("  T2 server  " + (shlex.join(server) if server else "(none: leave the destination down)"))
    emit("  T3 client  " + client_line)
    emit("\nLook for:")
    for line in recipe["look"]:
        emit("  " + line)
    return 0


def check_recipes():
    """Parse every recipe's peer arguments, so a recipe that no longer matches
    the CLI fails loudly. Used by scripts/integration_test.py."""
    parser = build_parser()
    for recipe in RECIPES:
        for role in ("server", "client"):
            if recipe[role] is None:
                continue
            try:
                prepare(parser.parse_args([role] + recipe[role]))
            except (StepError, SystemExit) as error:
                raise ValueError("recipe %r: bad %s arguments: %s"
                                 % (recipe["name"], role, error)) from None
        for line in recipe.get("stdin", "").splitlines():
            if line.startswith("/") and not line.startswith("//"):
                parse_step(line[1:])


# --- command line ----------------------------------------------------------------------


def build_parser():
    common = argparse.ArgumentParser(add_help=False)
    output = common.add_argument_group("output (same values as the proxy's options)")
    output.add_argument("-f", "--formatting", choices=list(FORMATS), default="lowerhex",
                        help="payload byte format (default: lowerhex)")
    output.add_argument("-s", "--separator", default=":",
                        help="byte separator (default: ':')")
    output.add_argument("-p", "--precision", choices=list(PRECISIONS), default="milliseconds",
                        help="timestamp precision (default: milliseconds)")
    output.add_argument("--no-text", action="store_true",
                        help="hide the quoted text preview after each payload")
    sending = common.add_argument_group("sending")
    sending.add_argument("--split", type=int, default=0, metavar="N",
                         help="send every payload in N-byte writes")
    sending.add_argument("--gap", type=float, default=0.0, metavar="S",
                         help="pause S seconds between those writes")

    parser = argparse.ArgumentParser(
        description="Scriptable TCP peers for manually testing logged_tcp_proxy.",
        epilog="Start with: python3 scripts/peer.py recipes",
    )
    commands = parser.add_subparsers(dest="command", required=True, metavar="COMMAND")
    formatter = argparse.RawDescriptionHelpFormatter

    server = commands.add_parser(
        "server", parents=[common], formatter_class=formatter, epilog=STEPS_HELP,
        help="the destination: accept connections from the proxy and answer",
        description="Accept connections (from the proxy) and answer them. Per connection:\n"
                    "run the --on-accept steps, reply to each received chunk according\n"
                    "to --mode, and once the other side has sent FIN, run the --on-eof\n"
                    "steps; then close gracefully.",
    )
    server.add_argument("-L", "--listen", default=SERVER_ADDR, metavar="ADDR",
                        help="listen address (default: %s, the proxy's -r)" % SERVER_ADDR)
    server.add_argument("--mode", choices=["prefix", "echo", "sink"], default="prefix",
                        help="prefix: reply --prefix + data (default; the directions "
                             "differ); echo: reply the data; sink: never reply")
    server.add_argument("--prefix", default="re: ", metavar="TEXT",
                        help="reply prefix for --mode prefix, escapes allowed "
                             "(default: 're: ')")
    server.add_argument("--on-accept", nargs="+", default=[], metavar="STEP",
                        help="steps to run on accept; a looping list replaces the replies")
    server.add_argument("--on-eof", nargs="+", default=[], metavar="STEP",
                        help="steps to run once the other side has sent FIN")
    server.add_argument("--once", action="store_true",
                        help="exit after the first connection has closed")

    client = commands.add_parser(
        "client", parents=[common], formatter_class=formatter, epilog=STEPS_HELP,
        help="connect to the proxy and talk through it",
        description="Connect (to the proxy) and run the STEPs; with no steps, send the\n"
                    "lines typed in the terminal (or piped in). Afterwards the connection\n"
                    "is closed gracefully unless a step said otherwise.",
    )
    client.add_argument("steps", nargs="*", metavar="STEP", help="steps to run (see below)")
    client.add_argument("-C", "--connect", default=PROXY_ADDR, metavar="ADDR",
                        help="address to connect to (default: %s, the proxy's -b)" % PROXY_ADDR)
    client.add_argument("--retry", type=float, default=15.0, metavar="S",
                        help="keep retrying a refused connection for S seconds "
                             "(default: 15)")
    client.add_argument("--eol", choices=["lf", "crlf", "none"], default="lf",
                        help="ending added to each typed line (default: lf)")

    recipes = commands.add_parser("recipes", help="list ready-made scenarios, or show one")
    recipes.add_argument("name", nargs="?", help="the recipe to show")
    return parser


def prepare(opts):
    """Validate and convert the parsed options in place; raises StepError."""
    if opts.command in ("server", "client"):
        if opts.split < 0 or opts.gap < 0:
            raise StepError("--split and --gap must not be negative")
    if opts.command == "server":
        parse_addr(opts.listen)
        opts.prefix = unescape(opts.prefix)
        opts.on_accept = parse_steps(opts.on_accept)
        opts.on_eof = parse_steps(opts.on_eof)
        if any(kind == "loop" for kind, _ in opts.on_eof):
            raise StepError("--on-eof cannot loop: the other side has stopped sending")
    elif opts.command == "client":
        parse_addr(opts.connect)
        opts.steps = parse_steps(opts.steps)
    return opts


def main(argv=None):
    parser = build_parser()
    opts = parser.parse_args(argv)
    try:
        prepare(opts)
    except StepError as error:
        parser.error(str(error))
    if opts.command == "recipes":
        return show_recipes(opts.name)
    log = Log(opts.command, opts)
    try:
        if opts.command == "server":
            return run_server(opts, log)
        return run_client(opts, log)
    except KeyboardInterrupt:
        return 0
    except OSError as error:  # e.g. the listen address is in use
        log.event(None, "!", describe(error))
        return 1


if __name__ == "__main__":
    sys.exit(main())
