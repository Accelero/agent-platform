"""Serve a test world over real TLS and call it as a client would."""

import http.client
import json
import ssl
import threading
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path

from runtime_manager.server import create_server

from tests.support.world import World


@dataclass
class Served:
    port: int
    certificate: Path
    token: str

    def connect(self) -> http.client.HTTPSConnection:
        context = ssl.create_default_context(cafile=str(self.certificate))
        return http.client.HTTPSConnection(
            "localhost", self.port, context=context, timeout=10
        )

    def call(self, method, path, body=None, *, token="", headers=None, raw=None):
        """One request; returns (status, response headers, decoded JSON body)."""
        connection = self.connect()
        try:
            sent = {"Authorization": "Bearer " + (token or self.token)}
            payload = raw
            if body is not None:
                payload = json.dumps(body).encode()
                sent["Content-Type"] = "application/json"
            elif method == "POST" and raw is not None:
                sent["Content-Type"] = "application/json"
            sent.update(headers or {})
            connection.request(method, path, body=payload, headers=sent)
            response = connection.getresponse()
            data = response.read()
            return response.status, dict(response.getheaders()), json.loads(data)
        finally:
            connection.close()


@contextmanager
def serve(world: World, tls: tuple[Path, Path], *, client: str = "hermes"):
    certificate, key = tls
    server = create_server(
        world.api, certfile=str(certificate), keyfile=str(key), host="127.0.0.1", port=0
    )
    thread = threading.Thread(
        target=lambda: server.serve_forever(poll_interval=0.01), daemon=True
    )
    thread.start()
    try:
        yield Served(server.server_address[1], certificate, world.tokens[client])
    finally:
        server.shutdown()
        server.server_close()
        thread.join(5)
