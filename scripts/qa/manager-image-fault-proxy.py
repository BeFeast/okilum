#!/usr/bin/env python3
"""Loopback-only upload fault injection and safe dispatch receipts for native QA.

Run beside an isolated T3 server on :3773, with OKILUM_IMAGE_QA_DIR pointing to
its disposable evidence directory. Pair the native client with :4773. Touch
fail-next-upload to reject one POST with HTTP 503; touch disconnect to close all
current connections. No credentials, signed URLs or message text are logged.
"""

import hashlib
import json
import os
import pathlib
import select
import socket
import threading
import time

q = pathlib.Path(os.environ["OKILUM_IMAGE_QA_DIR"]).resolve(strict=True)
assert (q / "project").is_dir(), "Use an isolated QA directory with project/"
active = set()
lock = threading.Lock()


def log(value):
    with lock:
        with (q / "proxy-receipts.jsonl").open("a") as file:
            file.write(json.dumps(value) + "\n")


class Frames:
    """Observe uncompressed client RPC frames without changing their delivery."""

    def __init__(self):
        self.buffer = bytearray()
        self.message = bytearray()

    def receive(self, data):
        self.buffer.extend(data)
        while len(self.buffer) >= 2:
            first, second = self.buffer[:2]
            length = second & 127
            offset = 2
            if length == 126:
                if len(self.buffer) < 4:
                    return
                length = int.from_bytes(self.buffer[2:4], "big")
                offset = 4
            elif length == 127:
                if len(self.buffer) < 10:
                    return
                length = int.from_bytes(self.buffer[2:10], "big")
                offset = 10
            assert length < 8 * 1024 * 1024, "bounded synthetic RPC frames only"
            mask_size = 4 if second & 128 else 0
            if len(self.buffer) < offset + mask_size + length:
                return
            mask = self.buffer[offset : offset + mask_size]
            payload = self.buffer[offset + mask_size : offset + mask_size + length]
            del self.buffer[: offset + mask_size + length]
            if mask:
                payload = bytes(
                    byte ^ mask[index % 4] for index, byte in enumerate(payload)
                )
            if first & 15 not in (0, 1):
                continue
            self.message.extend(payload)
            if not first & 128:
                continue
            request = json.loads(self.message)
            self.message.clear()
            if request.get("_tag") != "Request":
                continue
            tag = request.get("tag")
            value = request.get("payload", {})
            if tag == "orchestration.dispatchCommand":
                log(
                    dict(
                        event="dispatch",
                        type=value.get("type"),
                        threadId=value.get("threadId"),
                        commandId=value.get("commandId"),
                        messageId=value.get("messageId"),
                        attachmentIds=[a["id"] for a in value.get("attachments", [])],
                    )
                )
            elif tag == "attachments.createUploadUrl":
                log(
                    dict(
                        event="upload-ticket",
                        name=value.get("name"),
                        size=value.get("sizeBytes"),
                    )
                )
            elif tag == "assets.createUrl":
                log(
                    dict(
                        event="asset-ticket",
                        attachmentId=value.get("resource", {}).get("attachmentId"),
                    )
                )


def serve(client):
    upstream = None
    try:
        client.settimeout(5)
        head = b""
        while b"\r\n\r\n" not in head:
            chunk = client.recv(4096)
            if not chunk:
                return
            head += chunk
            assert len(head) < 65536, "bounded HTTP headers"
        header, body = head.split(b"\r\n\r\n", 1)
        method, url, _ = header.split(b"\r\n")[0].decode().split(" ")
        path = url.split("?")[0]
        websocket = path == "/ws"
        if method == "POST" and path.startswith("/api/attachments/upload/"):
            size = int(
                next(
                    line.split(b":", 1)[1]
                    for line in header.split(b"\r\n")
                    if line.lower().startswith(b"content-length:")
                )
            )
            assert size <= 50 * 1024 * 1024
            while len(body) < size:
                chunk = client.recv(min(65536, size - len(body)))
                if not chunk:
                    return
                body += chunk
            fail = (q / "fail-next-upload").exists()
            if fail:
                (q / "fail-next-upload").unlink()
            log(
                dict(
                    event="upload",
                    size=len(body),
                    sha256=hashlib.sha256(body).hexdigest(),
                    injected503=fail,
                )
            )
            if fail:
                client.sendall(
                    b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                return
        # A fresh HTTP connection per request makes every binary transfer
        # observable; WebSocket upgrades and frames remain byte-for-byte intact.
        if not websocket:
            header = (
                b"\r\n".join(
                    line
                    for line in header.split(b"\r\n")
                    if not line.lower().startswith(b"connection:")
                )
                + b"\r\nConnection: close"
            )
        upstream = socket.create_connection(("127.0.0.1", 3773))
        upstream.sendall(header + b"\r\n\r\n" + body)
        frames = Frames()
        client.setblocking(False)
        upstream.setblocking(False)
        with lock:
            active.update((client, upstream))
        while True:
            ready, _, _ = select.select([client, upstream], [], [], 0.25)
            for source in ready:
                data = source.recv(65536)
                if not data:
                    return
                if websocket and source is client:
                    frames.receive(data)
                target = upstream if source is client else client
                target.setblocking(True)
                target.sendall(data)
                target.setblocking(False)
    except (OSError, ValueError, StopIteration):
        pass
    finally:
        with lock:
            active.discard(client)
            active.discard(upstream)
        client.close()
        if upstream:
            upstream.close()


def control():
    while True:
        if (q / "disconnect").exists():
            (q / "disconnect").unlink()
            with lock:
                for connection in list(active):
                    try:
                        connection.shutdown(socket.SHUT_RDWR)
                    except OSError:
                        pass
            log(dict(event="disconnect"))
        time.sleep(0.1)


threading.Thread(target=control, daemon=True).start()
with socket.socket() as server:
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("127.0.0.1", 4773))
    server.listen()
    print("ISOLATED_PROXY_READY", flush=True)
    while True:
        client, _ = server.accept()
        threading.Thread(target=serve, args=(client,), daemon=True).start()
