"""TLS HTTP transport: authentication, body limits and the error model.

The TLS handshake runs in the connection's own thread. Done in the accept loop,
one client that opens a socket and stays silent would stall every other client.
"""
from __future__ import annotations

import json
import logging
import ssl
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from .api import ExecutionAPI, decode
from .domain.errors import ManagerError

log = logging.getLogger(__name__)
SOCKET_TIMEOUT = 10
MAX_BODY = 16384


def error_body(error: BaseException) -> tuple[int, dict, dict]:
    """The status, JSON body and extra headers of an exception."""
    if isinstance(error, ManagerError):
        body = {'error': str(error), 'code': error.code}
        if error.reason:
            body['reason'] = error.reason
        headers = {'Retry-After': str(error.retry_after)} if error.retry_after else {}
        return error.status, body, headers
    if isinstance(error, KeyError):
        return 404, {'error': 'Not found', 'code': 'not_found'}, {}
    if isinstance(error, (ValueError, TypeError, UnicodeError, TimeoutError)):
        return 400, {'error': 'Invalid request', 'code': 'invalid_request'}, {}
    log.error('Manager operation failed', exc_info=error)
    return 503, {'error': 'Provisioning unavailable', 'code': 'unavailable'}, {}


def create_server(api: ExecutionAPI, *, certfile: str, keyfile: str,
                  host: str = '0.0.0.0', port: int = 8090) -> ThreadingHTTPServer:
    class Handler(BaseHTTPRequestHandler):
        def setup(self):
            self.request.settimeout(SOCKET_TIMEOUT)
            self.request.do_handshake()
            super().setup()

        def log_message(self, *args):
            pass

        def respond(self, status, value, headers=None):
            body = json.dumps(value).encode()
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            for name, content in (headers or {}).items():
                self.send_header(name, content)
            self.end_headers()
            self.wfile.write(body)

        def handle_request(self, post=False):
            if not post and self.path == '/healthz':
                return self.respond(200, {'ok': True})
            client = api.authenticate(self.headers.get('Authorization', ''))
            if client is None:
                return self.respond(401, {'error': 'Unauthorized', 'code': 'unauthorized'})
            try:
                payload = None
                if post:
                    if (self.headers.get('Transfer-Encoding')
                            or self.headers.get('Content-Type', '').split(';')[0] != 'application/json'):
                        raise ValueError('JSON body required')
                    size = int(self.headers.get('Content-Length', '0'))
                    if not 0 < size <= MAX_BODY:
                        raise ValueError('Invalid body size')
                    body = self.rfile.read(size)
                    if len(body) != size:
                        raise ValueError('Incomplete body')
                    payload = decode(body)
                self.respond(200, api.dispatch(client, self.path, payload))
            except Exception as error:
                status, body, headers = error_body(error)
                self.respond(status, body, headers)

        def do_GET(self):
            self.handle_request()

        def do_POST(self):
            self.handle_request(post=True)

    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.load_cert_chain(certfile, keyfile)

    class TLSServer(ThreadingHTTPServer):
        daemon_threads = True

        def get_request(self):
            connection, address = super().get_request()
            return context.wrap_socket(connection, server_side=True,
                                       do_handshake_on_connect=False), address

        def handle_error(self, request, client_address):
            # A failed handshake or a dropped connection is routine, not an error.
            log.debug('Connection from %s ended: %r', client_address, sys.exc_info()[1])

    return TLSServer((host, port), Handler)
