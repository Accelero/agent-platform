"""Run the packaged gateway against an authenticated HTTP/WebSocket test server."""

import base64
import hashlib
import http.client
import json
import shutil
import socket
import subprocess
import threading
import time
import uuid
from contextlib import closing
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest
import yaml

from agent_platform.build import ROOT


class Backend(BaseHTTPRequestHandler):
    """Expose transport probes without running a model or accepting real credentials."""

    def do_GET(self) -> None:
        if self.headers.get("Cookie") != "session=fixture":
            self.send_error(401)
        elif self.path == "/api/ws":
            if (
                self.headers.get("Upgrade", "").lower() != "websocket"
                or self.headers.get("Connection", "").lower() != "upgrade"
            ):
                self.send_error(400)
                return
            key = self.headers["Sec-WebSocket-Key"]
            accept = base64.b64encode(
                hashlib.sha1(
                    (key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode(),
                    usedforsecurity=False,
                ).digest()
            ).decode()
            self.send_response(101)
            self.send_header("Upgrade", "websocket")
            self.send_header("Connection", "Upgrade")
            self.send_header("Sec-WebSocket-Accept", accept)
            self.end_headers()
        else:
            self.respond()

    def respond(self) -> None:
        if self.headers.get("Cookie") != "session=fixture":
            self.send_error(401)
            return
        data = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        body = json.dumps({"method": self.command, "bytes": len(data)}).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    do_POST = respond
    do_PUT = respond
    do_PATCH = respond
    do_DELETE = respond

    def log_message(self, *_args: object) -> None:
        pass


def request(port: int, method: str, path: str, body: bytes = b"", **headers):
    with closing(http.client.HTTPConnection("127.0.0.1", port, timeout=5)) as client:
        client.request(method, path, body=body, headers=headers)
        response = client.getresponse()
        return response.status, response.read()


@pytest.mark.skipif(shutil.which("docker") is None, reason="Docker is required")
def test_gateway_forwards_authenticated_dashboard_and_websocket_traffic(
    tmp_path: Path,
) -> None:
    """Exercise Nginx itself so a restrictive location cannot break dashboard auth."""
    chart = ROOT / "charts/agent-platform"
    image = yaml.safe_load((chart / "values.yaml").read_text())["workloads"][
        "hermes-gateway"
    ]["image"]
    name = "agent-platform-gateway-test-" + uuid.uuid4().hex
    backend = ThreadingHTTPServer(("127.0.0.1", 0), Backend)
    thread = threading.Thread(target=backend.serve_forever, daemon=True)
    thread.start()
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    config = (chart / "files/hermes-gateway.nginx.conf").read_text()
    config = config.replace("hermes:9119", "127.0.0.1:" + str(backend.server_port))
    config = config.replace("listen 8080;", f"listen 127.0.0.1:{port};")
    path = tmp_path / "nginx.conf"
    path.write_text(config)
    path.chmod(0o644)
    try:
        subprocess.run(
            [
                "docker",
                "run",
                "--detach",
                "--rm",
                "--name",
                name,
                "--network",
                "host",
                "--read-only",
                "--user",
                "101:101",
                "--cap-drop",
                "ALL",
                "--security-opt",
                "no-new-privileges",
                "--tmpfs",
                "/run/nginx:rw,uid=101,gid=101,size=64m",
                "--volume",
                str(path) + ":/etc/nginx/nginx.conf:ro",
                "--entrypoint",
                "nginx",
                image,
                "-g",
                "daemon off;",
            ],
            check=True,
            capture_output=True,
            timeout=120,
        )
        deadline = time.monotonic() + 20
        while True:
            try:
                assert request(port, "GET", "/api/auth/me")[0] == 401
                break
            except OSError:
                if time.monotonic() >= deadline:
                    raise
                time.sleep(0.1)
        headers = {"Cookie": "session=fixture"}
        for method, route in (
            ("POST", "/api/auth/ws-ticket"),
            ("POST", "/auth/logout"),
            ("PUT", "/api/config"),
            ("PATCH", "/api/sessions/fixture"),
            ("DELETE", "/api/sessions/fixture"),
        ):
            status, payload = request(port, method, route, **headers)
            assert status == 200 and json.loads(payload)["method"] == method
        status, payload = request(
            port, "POST", "/api/upload", b"x" * (2 * 1024 * 1024), **headers
        )
        assert status == 200 and json.loads(payload)["bytes"] == 2 * 1024 * 1024
        status, _payload = request(
            port,
            "GET",
            "/api/ws",
            **headers,
            Upgrade="websocket",
            Connection="Upgrade",
            **{"Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ=="},
        )
        assert status == 101
    finally:
        subprocess.run(
            ["docker", "rm", "--force", name], capture_output=True, timeout=30
        )
        backend.shutdown()
        backend.server_close()
        thread.join(timeout=5)
