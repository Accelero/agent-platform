"""Run the routing plugin inside pinned Traefik with ephemeral mTLS fixtures.

This proves proxy ordering, Yaegi loading and active connection closure. Identity
providers and native Hermes are fixtures here; their OIDC/native acceptance
tests remain separate gates. No deployment or public hostname is contacted.

Run: uv run python tests/personal_hermes/router_compatibility.py
"""

from __future__ import annotations

import base64
import contextlib
import hashlib
import http.client
import json
import os
import socket
import ssl
import struct
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PLUGIN_MODULE = "github.com/accelero/hermesrouteresolver"
TRAEFIK = (
    "traefik@sha256:0f6150202bc1fed72a7bd2107460eaa510e5b8a905f3fdb5b84f2c8619c33368"
)
ISSUER = "https://sso.example.org/realms/agents"
PREFIX = "a" * 24
OWNERS = {
    "owner-a": "00000000-0000-4000-8000-000000000001",
    "owner-b": "00000000-0000-4000-8000-000000000002",
}
VERIFIED = [
    "X-Hermes-Verified-Issuer",
    "X-Hermes-Verified-Subject",
    "X-Hermes-Verified-Evidence",
    "X-Hermes-Verified-Session",
]
ROUTING = ["X-Hermes-Instance", "X-Hermes-Route-Generation"]


class State:
    def __init__(self):
        self.lock = threading.Lock()
        self.revoked = False
        self.session_revoked = False
        self.manager_outage = False
        self.switch_identity = False
        self.generation = 1
        self.manager_calls = 0


def run(args, *, timeout=30, **kwargs):
    return subprocess.run(
        args, check=True, timeout=timeout, capture_output=True, text=True, **kwargs
    )


def certificates(directory: Path):
    run(
        [
            "openssl",
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            str(directory / "tls.key"),
            "-out",
            str(directory / "tls.crt"),
            "-days",
            "1",
            "-subj",
            "/CN=hermes-router-fixture",
            "-addext",
            "subjectAltName=IP:127.0.0.1,DNS:localhost",
            "-addext",
            "extendedKeyUsage=serverAuth,clientAuth",
        ]
    )
    os.chmod(directory / "tls.key", 0o600)


def handler(state: State, backend: str | None = None):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *_args):
            pass

        def json(self, status, body):
            encoded = json.dumps(body).encode()
            self.send_response(status)
            self.send_header("Cache-Control", "no-store")
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(encoded)))
            self.end_headers()
            self.wfile.write(encoded)

        def do_GET(self):  # noqa: N802
            if self.path == "/forward-auth":
                # ForwardAuth only receives Cookie, never browser identity or
                # backend-selection headers. Cookies select FIXTURE sessions.
                assert not any(self.headers.get(key) for key in VERIFIED + ROUTING)
                owner = self.headers.get("Cookie", "").removeprefix("fixture-owner=")
                if owner not in OWNERS:
                    self.json(403, {"error": "access_denied"})
                    return
                with state.lock:
                    denied = state.session_revoked
                if denied:
                    self.json(401, {"error": "session_expired"})
                    return
                self.send_response(200)
                self.send_header(VERIFIED[0], ISSUER)
                self.send_header(VERIFIED[1], owner)
                self.send_header(VERIFIED[2], "fixture-token-" + owner)
                self.send_header(VERIFIED[3], ("c" if owner == "owner-a" else "d") * 64)
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
            assert backend is not None
            assert not any(self.headers.get(key) for key in VERIFIED + ROUTING)
            if self.headers.get("Upgrade", "").lower() == "websocket":
                accept = base64.b64encode(
                    hashlib.sha1(
                        (
                            self.headers["Sec-WebSocket-Key"]
                            + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
                        ).encode(),
                        usedforsecurity=False,
                    ).digest()
                ).decode()
                self.send_response(101)
                self.send_header("Connection", "Upgrade")
                self.send_header("Upgrade", "websocket")
                self.send_header("Sec-WebSocket-Accept", accept)
                self.end_headers()
                self.connection.settimeout(20)
                while True:
                    try:
                        data = self.rfile.read(2)
                        if not data:
                            return
                        assert data[0] == 0x81 and data[1] == 0x85
                        mask = self.rfile.read(4)
                        payload = self.rfile.read(5)
                        unmasked = bytes(v ^ mask[i % 4] for i, v in enumerate(payload))
                        assert unmasked == b"hello"
                        self.wfile.write(b"\x81\x05hello")
                        self.wfile.flush()
                    except OSError, ConnectionError:
                        return
            else:
                self.json(200, {"backend": backend})

        def do_POST(self):  # noqa: N802
            assert self.connection.getpeercert()
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            with state.lock:
                if self.path == "/internal/v1/sessions/authorize":
                    if state.session_revoked:
                        self.json(401, {"error": "session_expired"})
                        return
                    owner = "owner-a" if body["sessionId"] == "c" * 64 else "owner-b"
                    if state.switch_identity:
                        owner = "owner-b"
                    self.json(
                        200,
                        {
                            "identity": {"issuer": ISSUER, "subject": owner},
                            "accessToken": "fixture-token-" + owner,
                        },
                    )
                    return
                assert self.path == "/internal/v1/routes/resolve"
                state.manager_calls += 1
                if state.manager_outage:
                    self.json(
                        503,
                        {"status": "unavailable", "reason": "dependency_unavailable"},
                    )
                    return
                owner = body["identity"]["subject"]
                if (
                    body["identity"]["issuer"] != ISSUER
                    or self.headers["Authorization"] != "Bearer fixture-token-" + owner
                ):
                    self.json(401, {"error": "invalid_identity_evidence"})
                    return
                if (state.revoked and owner == "owner-a") or owner not in OWNERS:
                    self.json(403, {"status": "denied", "reason": "access_denied"})
                    return
                instance = OWNERS[owner]
                self.json(
                    200,
                    {
                        "status": "ready",
                        "instanceId": instance,
                        "routeGeneration": state.generation,
                        "authorizationVersion": 1,
                        "destination": {
                            "namespace": f"hi-{PREFIX}-{instance.replace('-', '')}",
                            "service": "hermes",
                            "port": 8080,
                            "scheme": "http",
                        },
                    },
                )

    return Handler


@contextlib.contextmanager
def server(state, directory, backend=None):
    instance = ThreadingHTTPServer(("127.0.0.1", 0), handler(state, backend))
    instance.daemon_threads = True
    if backend is None:
        tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        tls.load_cert_chain(directory / "tls.crt", directory / "tls.key")
        tls.load_verify_locations(directory / "tls.crt")
        tls.verify_mode = ssl.CERT_REQUIRED
        instance.socket = tls.wrap_socket(instance.socket, server_side=True)
    thread = threading.Thread(target=instance.serve_forever, daemon=True)
    thread.start()
    try:
        yield instance.server_port
    finally:
        instance.shutdown()
        instance.server_close()
        thread.join(timeout=5)


def snapshot(directory, auth_port, backends, generation=1):
    remove = {key: "" for key in VERIFIED + ROUTING}
    config = {
        "http": {
            "routers": {
                "parent": {
                    "rule": "Host(`hermes.example.org`)",
                    "entryPoints": ["web"],
                    "middlewares": ["remove-input", "authenticate", "resolve"],
                },
                **{
                    owner: {
                        "parentRefs": ["parent"],
                        "rule": (
                            f"Header(`X-Hermes-Instance`, `{OWNERS[owner]}`) && "
                            f"Header(`X-Hermes-Route-Generation`, `{generation}`)"
                        ),
                        "middlewares": ["remove-internal"],
                        "service": owner,
                    }
                    for owner in OWNERS
                },
            },
            "middlewares": {
                "remove-input": {"headers": {"customRequestHeaders": remove}},
                "remove-internal": {"headers": {"customRequestHeaders": remove}},
                "authenticate": {
                    "forwardAuth": {
                        "address": f"https://127.0.0.1:{auth_port}/forward-auth",
                        "authRequestHeaders": ["Cookie"],
                        "authResponseHeaders": VERIFIED,
                        "trustForwardHeader": False,
                        "tls": {
                            "ca": "/fixture/tls.crt",
                            "cert": "/fixture/tls.crt",
                            "key": "/fixture/tls.key",
                        },
                    }
                },
                "resolve": {
                    "plugin": {
                        "hermesresolver": {
                            "managerURL": f"https://127.0.0.1:{auth_port}/internal/v1/routes/resolve",
                            "sessionURL": f"https://127.0.0.1:{auth_port}/internal/v1/sessions/authorize",
                            "issuer": ISSUER,
                            "installationPrefix": PREFIX,
                            "caFile": "/fixture/tls.crt",
                            "certFile": "/fixture/tls.crt",
                            "keyFile": "/fixture/tls.key",
                        }
                    }
                },
            },
            "services": {
                owner: {
                    "loadBalancer": {"servers": [{"url": f"http://127.0.0.1:{port}"}]}
                }
                for owner, port in backends.items()
            },
        }
    }
    # JSON is valid YAML; the directory provider filters by YAML/TOML suffix.
    path = directory / "routes" / "dynamic.yaml"
    temporary = directory / "dynamic.next"
    temporary.write_text(json.dumps(config))
    temporary.replace(path)


def fetch(port, owner, headers=None):
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=3)
    try:
        connection.request(
            "GET",
            "/api/settings",
            headers={
                "Host": "hermes.example.org",
                "Cookie": "fixture-owner=" + owner,
                **(headers or {}),
            },
        )
        response = connection.getresponse()
        return response.status, response.read()
    finally:
        connection.close()


def connect_websocket(port):
    connection = socket.create_connection(("127.0.0.1", port), timeout=3)
    key = base64.b64encode(b"fixture-ws-key-16").decode()
    connection.sendall(
        (
            "GET /api/ws HTTP/1.1\r\nHost: hermes.example.org\r\n"
            "Cookie: fixture-owner=owner-a\r\nConnection: Upgrade\r\n"
            "Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n"
            f"Sec-WebSocket-Key: {key}\r\n\r\n"
        ).encode()
    )
    header = bytearray()
    while not header.endswith(b"\r\n\r\n"):
        byte = connection.recv(1)
        assert byte, "connection closed before upgrade"
        header.extend(byte)
        assert len(header) <= 16384
    assert header.startswith(b"HTTP/1.1 101"), header.splitlines()[0]
    # Send a valid masked RFC6455 text frame and verify the proxied response.
    mask = b"mask"
    frame = bytes(v ^ mask[i % 4] for i, v in enumerate(b"hello"))
    connection.sendall(struct.pack("BB", 0x81, 0x85) + mask + frame)
    response = bytearray()
    while len(response) < 7:
        chunk = connection.recv(7 - len(response))
        assert chunk, "connection closed before WebSocket echo"
        response.extend(chunk)
    assert response == b"\x81\x05hello"
    return connection


def main():
    state = State()
    with tempfile.TemporaryDirectory(
        prefix="hermes-router-compatibility-"
    ) as temporary:
        directory = Path(temporary)
        certificates(directory)
        (directory / "routes").mkdir()
        with contextlib.ExitStack() as stack:
            auth_port = stack.enter_context(server(state, directory))
            backends = {
                owner: stack.enter_context(server(state, directory, owner))
                for owner in OWNERS
            }
            with socket.socket() as port_socket:
                port_socket.bind(("127.0.0.1", 0))
                proxy_port = port_socket.getsockname()[1]
            snapshot(directory, auth_port, backends)
            (directory / "traefik.json").write_text(
                json.dumps(
                    {
                        "entryPoints": {"web": {"address": f"127.0.0.1:{proxy_port}"}},
                        "providers": {
                            "file": {"directory": "/fixture/routes", "watch": True},
                            "providersThrottleDuration": "100ms",
                        },
                        "experimental": {
                            "localPlugins": {
                                "hermesresolver": {"moduleName": PLUGIN_MODULE}
                            }
                        },
                        "log": {"level": "ERROR"},
                    }
                )
            )
            name = "hermes-router-compatibility-" + str(os.getpid())
            run(
                [
                    "docker",
                    "run",
                    "--detach",
                    "--rm",
                    "--name",
                    name,
                    "--network",
                    "host",
                    "--user",
                    f"{os.getuid()}:{os.getgid()}",
                    "--cap-drop",
                    "ALL",
                    "--security-opt",
                    "no-new-privileges",
                    "--read-only",
                    "--tmpfs",
                    "/tmp",
                    "--mount",
                    f"type=bind,src={directory},dst=/fixture,readonly",
                    "--mount",
                    (
                        f"type=bind,src={ROOT / 'plugins/hermes-route-resolver'},"
                        f"dst=/plugins-local/src/{PLUGIN_MODULE},readonly"
                    ),
                    TRAEFIK,
                    "--configFile=/fixture/traefik.json",
                ],
                timeout=60,
            )
            try:
                deadline = time.monotonic() + 30
                last_response = None
                while time.monotonic() < deadline:
                    try:
                        last_response = fetch(proxy_port, "owner-a")
                        if last_response[0] == 200:
                            break
                    except OSError, http.client.HTTPException:
                        pass
                    time.sleep(0.1)
                else:
                    # Only fixture logs: no real credentials, tokens or chats.
                    logs = run(["docker", "logs", name]).stdout
                    raise AssertionError(
                        "Traefik startup/Yaegi loading failed: "
                        f"response={last_response!r}, lookups={state.manager_calls}; "
                        + logs[-8000:]
                    )
                for owner in OWNERS:
                    status, body = fetch(
                        proxy_port, owner, {key: "forged" for key in VERIFIED + ROUTING}
                    )
                    assert status == 200 and json.loads(body)["backend"] == owner
                assert (
                    fetch(
                        proxy_port,
                        "unassigned",
                        {key: "forged" for key in VERIFIED + ROUTING},
                    )[0]
                    == 403
                )
                assert (
                    fetch(proxy_port, "owner-a", {"Host": "foreign.example.org"})[0]
                    == 404
                )
                with state.lock:
                    initial_calls = state.manager_calls
                for _ in range(3):
                    assert fetch(proxy_port, "owner-a")[0] == 200
                with state.lock:
                    assert state.manager_calls == initial_calls + 3
                print(
                    "PASS: Yaegi startup, authentication ordering, two owners, "
                    "forged headers, no positive cache",
                    flush=True,
                )

                for mode in [
                    "revoked",
                    "session_revoked",
                    "switch_identity",
                    "manager_outage",
                ]:
                    with connect_websocket(proxy_port) as connection:
                        started = time.monotonic()
                        with state.lock:
                            setattr(state, mode, True)
                        if mode == "revoked":
                            assert fetch(proxy_port, "owner-a")[0] == 403
                            assert fetch(proxy_port, "owner-b")[0] == 200
                        connection.settimeout(10)
                        assert connection.recv(1) == b"", (
                            "revoked WebSocket remained usable"
                        )
                        elapsed = time.monotonic() - started
                        assert elapsed < 10, "revocation exceeded specification bound"
                        print(
                            f"PASS: established WebSocket closes for {mode} "
                            f"in {elapsed:.2f}s",
                            flush=True,
                        )
                    with state.lock:
                        setattr(state, mode, False)

                # The file provider watches a DIRECTORY and reloads an atomic
                # snapshot. A mismatch cannot fall back to a different owner.
                snapshot(directory, auth_port, backends, generation=2)
                deadline = time.monotonic() + 5
                while (
                    time.monotonic() < deadline
                    and fetch(proxy_port, "owner-a")[0] != 404
                ):
                    time.sleep(0.1)
                assert fetch(proxy_port, "owner-a")[0] == 404
                with state.lock:
                    state.generation = 2
                assert (
                    json.loads(fetch(proxy_port, "owner-b")[1])["backend"] == "owner-b"
                )
                print(
                    "PASS: directory-mounted atomic reload and "
                    "stale route generation denial",
                    flush=True,
                )
            finally:
                run(["docker", "kill", name])


if __name__ == "__main__":
    main()
