"""Isolated Feedback TLS inspection fixture. Loopback only; never forwards."""

import http.server
import json
from pathlib import Path
import ssl
import sys
import threading


def main():
    directory = Path(sys.argv[1])
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(directory / "leaf.pem", directory / "leaf.key")
    lock = threading.Lock()

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *_args):
            pass

        def record(self, **fields):
            with lock, (directory / "requests.jsonl").open("a", encoding="utf-8") as out:
                out.write(json.dumps({"control": self.control(), **fields}) + "\n")

        def control(self):
            return (directory / "control.txt").read_text(encoding="utf-8-sig").strip()

        def reply(self, status, body, kind="application/json"):
            self.send_response(status)
            self.send_header("Content-Type", kind)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(body)
            self.close_connection = True

        def do_CONNECT(self):
            self.record(method="CONNECT", target=self.path)
            if self.path.lower() != "feedback.okilum.test:443":
                self.reply(403, b"{}")
                return
            if self.control() == "proxy-auth":
                self.send_response(407, "Proxy Authentication Required")
                self.send_header("Proxy-Authenticate", "Negotiate")
                self.send_header("Content-Length", "0")
                self.send_header("Connection", "close")
                self.end_headers()
                self.close_connection = True
                return
            self.send_response(200, "Connection established")
            self.end_headers()
            self.close_connection = True
            try:
                with context.wrap_socket(self.connection, server_side=True) as stream:
                    Handler(stream, self.client_address, self.server)
            except (OSError, ssl.SSLError) as error:
                self.record(method="TLS-rejected", reason=str(error))

        def do_GET(self):
            if self.path != "/proxy.pac":
                self.reply(404, b"{}")
                return
            self.record(method="PAC", target=self.path)
            self.reply(200, (
                'function FindProxyForURL(url, host) {'
                'if (host == "feedback.okilum.test") return "PROXY 127.0.0.1:%d";'
                'return "DIRECT";}' % proxy.server_port
            ).encode(), "application/x-ns-proxy-autoconfig")

        def do_POST(self):
            # No valid test payload can escape this process, even if proxy
            # settings are wrong. Reserved .test never resolves to production.
            if self.path != "/api/feedback/okilum" or not isinstance(self.connection, ssl.SSLSocket):
                self.reply(403, b"{}")
                return
            length = int(self.headers.get("Content-Length", "0"))
            if not 0 < length <= 16384:
                self.reply(413, b"{}")
                return
            body = self.rfile.read(length)
            multipart = self.headers.get("Content-Type", "").startswith("multipart/form-data;")
            valid = multipart and all(field in body for field in [
                b'name="text"', b'name="kind"', b'name="meta"',
                "A report with Ελληνικά and an idea 💡".encode(),
            ])
            self.record(method="POST", target=self.path, host=self.headers.get("Host"),
                        user_agent=self.headers.get("User-Agent"), valid_multipart=valid)
            if self.control() == "blocked":
                self.reply(403, b"<html>Corporate policy block</html>", "text/html")
            elif not valid:
                self.reply(400, b'{}')
            else:
                self.reply(201, json.dumps({"issue_number": 123, "issue_url":
                    "https://git.oklabs.uk/BeFeast/okilum/issues/123"}).encode())

    class Server(http.server.ThreadingHTTPServer):
        daemon_threads = True

        def get_request(self):
            stream, address = super().get_request()
            stream.settimeout(10)
            return stream, address

    proxy = Server(("127.0.0.1", 0), Handler)
    direct = Server(("127.0.0.1", 0), Handler)
    direct.socket = context.wrap_socket(direct.socket, server_side=True)
    threading.Thread(target=direct.serve_forever, daemon=True).start()
    (directory / "ready.json").write_text(json.dumps({
        "proxy_port": proxy.server_port, "direct_port": direct.server_port,
        "forwarding": False,
    }), encoding="utf-8")
    proxy.serve_forever()


if __name__ == "__main__":
    main()
