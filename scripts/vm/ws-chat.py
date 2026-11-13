#!/usr/bin/env python3
"""The host side of serve-test.sh: a WebSocket client of a `triad serve`
in the guest. Standard library only (RFC 6455 by hand).

    ws-chat.py <host:port> <token-file> <invite> <host-line> <reply-line>

Connects (retrying while the guest boots), joins the channel in the invite
with a `command` request, waits to be let in, waits for <host-line> from
the host, sends <reply-line> with a `send` request, and exits 0. Every
event is printed as one JSON line.
"""
import base64
import json
import os
import socket
import struct
import sys
import time

addr, token_file, invite, host_line, reply_line = sys.argv[1:6]
host, port = addr.rsplit(":", 1)
token = open(token_file).read().strip()
DEADLINE = time.time() + float(os.environ.get("WS_TIMEOUT", "240"))


def connect():
    while True:
        try:
            s = socket.create_connection((host, int(port)), timeout=5)
            key = base64.b64encode(os.urandom(16)).decode()
            s.sendall(
                (
                    f"GET /v1 HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\n"
                    f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n"
                    f"Sec-WebSocket-Version: 13\r\nAuthorization: Bearer {token}\r\n\r\n"
                ).encode()
            )
            head = b""
            while not head.endswith(b"\r\n\r\n"):
                b = s.recv(1)
                if not b:
                    raise OSError("closed during the handshake")
                head += b
            if b" 101 " not in head.split(b"\r\n")[0]:
                sys.exit(f"refused: {head.splitlines()[0]!r}")
            s.settimeout(None)
            return s
        except OSError as e:
            if time.time() > DEADLINE:
                sys.exit(f"could not connect: {e}")
            time.sleep(1)


def read_exact(s, n):
    out = b""
    while len(out) < n:
        b = s.recv(n - len(out))
        if not b:
            raise EOFError
        out += b
    return out


def write_frame(s, opcode, payload):
    mask = os.urandom(4)
    n = len(payload)
    if n < 126:
        head = struct.pack("!BB", 0x80 | opcode, 0x80 | n)
    elif n < 65536:
        head = struct.pack("!BBH", 0x80 | opcode, 0x80 | 126, n)
    else:
        head = struct.pack("!BBQ", 0x80 | opcode, 0x80 | 127, n)
    s.sendall(head + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(payload)))


def next_event(s):
    data, opcode = b"", 0
    while True:
        b0, b1 = read_exact(s, 2)
        n = b1 & 0x7F
        if n == 126:
            n = struct.unpack("!H", read_exact(s, 2))[0]
        elif n == 127:
            n = struct.unpack("!Q", read_exact(s, 8))[0]
        part = read_exact(s, n)
        op = b0 & 0x0F
        if op == 9:
            write_frame(s, 10, part)
            continue
        if op == 8:
            raise EOFError(f"closed: {part[:2].hex()} {part[2:].decode(errors='replace')}")
        if op >= 8:
            continue
        if op:
            opcode = op
        data += part
        if b0 & 0x80 and opcode == 1:
            ev = json.loads(data)
            print(json.dumps(ev), flush=True)
            return ev
        if b0 & 0x80:
            data, opcode = b"", 0


def wait_for(s, what, pred):
    while time.time() < DEADLINE:
        ev = next_event(s)
        if pred(ev):
            return ev
    sys.exit(f"timed out waiting for {what}")


s = connect()
ready = wait_for(s, "ready", lambda e: e["event"] == "ready")
me = ready["node"]
write_frame(s, 1, json.dumps({"id": 1, "op": "command", "line": f"/join {invite}"}).encode())
added = wait_for(s, "the channel", lambda e: e["event"] == "channel")
done = wait_for(s, "done", lambda e: e["event"] == "done" and e.get("re") == 1)
if not done["ok"]:
    sys.exit(f"join failed: {done}")
channel = added["channel"]
wait_for(s, "being let in", lambda e: e["event"] == "joined" and e["member"] == me and e["channel"] == channel)
wait_for(s, "the host's line", lambda e: e["event"] == "message" and e["text"] == host_line)
write_frame(s, 1, json.dumps({"id": 2, "op": "send", "channel": channel, "text": reply_line}).encode())
done = wait_for(s, "done", lambda e: e["event"] == "done" and e.get("re") == 2)
if not done["ok"]:
    sys.exit(f"send failed: {done}")
wait_for(s, "our own message", lambda e: e["event"] == "message" and e["text"] == reply_line)
print("WS_CHAT_OK", flush=True)
