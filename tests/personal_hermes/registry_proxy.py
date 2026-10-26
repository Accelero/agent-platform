"""Basic authentication in front of a plain registry for the end-to-end run.

The stock registry image needs bcrypt for its own basic authentication; this
proxy compares the `Authorization` header with the fixture credentials and
forwards everything else unchanged, keeping the `Host` header so the
registry's upload locations point back here.

Standard library only: it runs in the Hermes image with its Python. The
credentials come from the `CREDENTIALS` environment variable as `user:password`.
"""

import base64
import http.client
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

UPSTREAM = os.environ["UPSTREAM"]
EXPECTED = "Basic " + base64.b64encode(os.environ["CREDENTIALS"].encode()).decode()
HOP = {"authorization", "connection", "content-length", "transfer-encoding"}


def read_chunked(stream):
    body = bytearray()
    while True:
        size = int(stream.readline().split(b";")[0].strip() or b"0", 16)
        if size == 0:
            stream.readline()
            return bytes(body)
        body += stream.read(size)
        stream.readline()


class Proxy(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def forward(self):
        if self.headers.get("Authorization") != EXPECTED:
            self.send_response(401)
            self.send_header("WWW-Authenticate", 'Basic realm="e2e"')
            self.send_header("Docker-Distribution-Api-Version", "registry/2.0")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if "chunked" in (self.headers.get("Transfer-Encoding") or "").lower():
            body = read_chunked(self.rfile)
        elif self.headers.get("Content-Length"):
            body = self.rfile.read(int(self.headers["Content-Length"]))
        else:
            body = None
        headers = {k: v for k, v in self.headers.items() if k.lower() not in HOP}
        if body is not None:
            headers["Content-Length"] = str(len(body))
        upstream = http.client.HTTPConnection(UPSTREAM, timeout=300)
        upstream.request(self.command, self.path, body=body, headers=headers)
        reply = upstream.getresponse()
        data = reply.read()
        self.send_response(reply.status)
        for key, value in reply.getheaders():
            if key.lower() not in HOP:
                self.send_header(key, value)
        # A HEAD reply announces the blob's size without a body.
        length = reply.getheader("Content-Length") if self.command == "HEAD" else None
        self.send_header("Content-Length", length or str(len(data)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(data)
        upstream.close()

    do_GET = do_HEAD = do_POST = do_PUT = do_PATCH = do_DELETE = forward


if __name__ == "__main__":
    ThreadingHTTPServer(("0.0.0.0", 5000), Proxy).serve_forever()
