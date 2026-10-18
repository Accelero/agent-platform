"""Authenticated scope API; clients never submit worker specifications."""
import hashlib
import hmac
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import logging
import os
from pathlib import Path
import re
import ssl
import threading

from manager import Conflict, Forbidden, Manager, MODES

log = logging.getLogger(__name__)
IDENTIFIER = re.compile(r'[A-Za-z0-9][A-Za-z0-9_.:-]{0,255}')


def identifier(value):
    if not isinstance(value, str) or not IDENTIFIER.fullmatch(value):
        raise ValueError('Invalid scope identifier')
    return value


def strict_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('Duplicate JSON field')
        result[key] = value
    return result


class ExecutionAPI:
    def __init__(self, manager, credentials):
        self.manager = manager
        self.clients = manager.config['clients']
        if not self.clients or set(credentials) != set(self.clients):
            raise ValueError('Every configured client needs a separate credential')
        if any(not isinstance(token, str) or len(token) < 32 for token in credentials.values()):
            raise ValueError('Client credentials must have at least 32 characters')
        if len(set(credentials.values())) != len(credentials):
            raise ValueError('Client credentials must be distinct')
        self.tokens = {name: hashlib.sha256(token.encode()).digest() for name, token in credentials.items()}
        deployments = []
        for client in self.clients.values():
            deployment = client['deployment']
            profiles = manager.config['deployments'][deployment]['profiles']
            if '*' in profiles or not profiles:
                raise ValueError('Clients require explicit approved profiles')
            for profile in profiles.values():
                if len(profile['templates']) != 1 or profile['templates'][0] not in manager.config['templates']:
                    raise ValueError('Each profile must select one administrator-defined template')
            deployments.append(deployment)
        if len(set(deployments)) != len(deployments):
            raise ValueError('Each client must have its own deployment scope')

    def authenticate(self, authorization):
        if not authorization.startswith('Bearer '):
            return None
        digest = hashlib.sha256(authorization[7:].encode()).digest()
        for name, expected in self.tokens.items():
            if hmac.compare_digest(digest, expected):
                return name
        return None

    def dispatch(self, client, path, payload=None):
        deployment = self.clients[client]['deployment']
        if path == '/v1/environments' and payload is None:
            return self.manager.status(deployment=deployment)
        fields = {
            '/v1/leases/acquire': ({'profile', 'session', 'mode', 'request'}, {'runtime', 'parent_lease'}),
            '/v1/leases/heartbeat': ({'lease'}, set()),
            '/v1/leases/release': ({'lease'}, set()),
            '/v1/sessions/close': ({'profile', 'session'}, {'runtime'}),
        }
        if path not in fields:
            raise KeyError('Unknown endpoint')
        required, optional = fields[path]
        if not isinstance(payload, dict) or not required <= payload.keys() or payload.keys() - required - optional:
            raise ValueError('Invalid request fields')
        if 'lease' in payload:
            lease = identifier(payload['lease'])
            if path.endswith('/heartbeat'):
                return self.manager.heartbeat(lease, deployment=deployment)
            return self.manager.release(lease, deployment=deployment, reconcile=False)
        profile, session = identifier(payload['profile']), identifier(payload['session'])
        allowed = self.manager.config['deployments'][deployment]['profiles'].get(profile)
        if not allowed:
            raise Forbidden('Profile is not approved for this client')
        if path == '/v1/sessions/close':
            runtime = payload.get('runtime')
            if runtime is not None and not isinstance(runtime, dict):
                raise ValueError('Invalid invocation scope')
            return self.manager.close_session(deployment, profile, session, runtime=runtime, reconcile=False)
        mode = payload['mode']
        if not isinstance(mode, str) or mode not in MODES:
            raise ValueError('Invalid scope lifetime')
        request = identifier(payload['request'])
        if 'parent_lease' in payload:
            identifier(payload['parent_lease'])
        # Idempotency keys are local to the authenticated client.
        selected = dict(payload, deployment=deployment, template=allowed['templates'][0],
                        request=hashlib.sha256(json.dumps([deployment, request]).encode()).hexdigest())
        return self.manager.acquire(selected)


def create_server(manager, credentials, *, certfile, keyfile, host='0.0.0.0', port=8090):
    api = ExecutionAPI(manager, credentials)

    class Handler(BaseHTTPRequestHandler):
        def setup(self):
            super().setup()
            self.connection.settimeout(10)

        def log_message(self, *args):
            pass

        def respond(self, status, value):
            body = json.dumps(value).encode()
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def handle_request(self, post=False):
            if not post and self.path == '/healthz':
                return self.respond(200, {'ok': True})
            client = api.authenticate(self.headers.get('Authorization', ''))
            if client is None:
                return self.respond(401, {'error': 'Unauthorized'})
            try:
                payload = None
                if post:
                    if self.headers.get('Transfer-Encoding') or self.headers.get('Content-Type', '').split(';')[0] != 'application/json':
                        raise ValueError('JSON body required')
                    size = int(self.headers.get('Content-Length', '0'))
                    if not 0 < size <= 16384:
                        raise ValueError('Invalid body size')
                    body = self.rfile.read(size)
                    if len(body) != size:
                        raise ValueError('Incomplete body')
                    payload = json.loads(body, object_pairs_hook=strict_object)
                elif self.path != '/v1/environments':
                    raise KeyError('Unknown endpoint')
                self.respond(200, api.dispatch(client, self.path, payload))
            except Forbidden:
                self.respond(403, {'error': 'Scope is not authorized for this client'})
            except Conflict as error:
                self.respond(409, {'error': str(error)})
            except KeyError:
                self.respond(404, {'error': 'Not found'})
            except (ValueError, TypeError, UnicodeError, TimeoutError):
                self.respond(400, {'error': 'Invalid request'})
            except Exception:
                log.exception('Manager operation failed')
                self.respond(503, {'error': 'Provisioning unavailable'})

        def do_GET(self):
            self.handle_request()

        def do_POST(self):
            self.handle_request(post=True)

    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.load_cert_chain(certfile, keyfile)
    class TLSServer(ThreadingHTTPServer):
        def get_request(self):
            connection, address = super().get_request()
            connection.settimeout(10)
            try:
                return context.wrap_socket(connection, server_side=True), address
            except OSError:
                connection.close()
                raise

    server = TLSServer((host, port), Handler)
    server.daemon_threads = True
    return server


def serve(config, backend, database, credentials, *, certfile, keyfile):
    manager = Manager(config, backend, database)
    server = create_server(manager, credentials, certfile=certfile, keyfile=keyfile)
    stop = threading.Event()

    def maintenance():
        while not stop.is_set():
            try:
                manager.maintain()
            except Exception:
                log.exception('Manager maintenance failed')
            stop.wait(config.get('maintenance_interval_seconds', 10))

    worker = threading.Thread(target=maintenance, daemon=True)
    worker.start()
    try:
        server.serve_forever()
    finally:
        stop.set()
        server.server_close()


if __name__ == '__main__':
    import yaml
    from backends import KubernetesBackend
    logging.basicConfig(level=logging.INFO)
    config = yaml.safe_load(Path(os.environ.get('MANAGER_CONFIG', '/etc/execution-manager/config.yaml')).read_text())
    if config.get('backend') != 'kubernetes':
        raise ValueError('Only the Kubernetes backend is supported')
    credentials = {name: Path(client['token_file']).read_text().strip() for name, client in config['clients'].items()}
    serve(config, KubernetesBackend(config['kubernetes']), os.environ.get('MANAGER_DATABASE', '/state/manager.sqlite3'),
          credentials, certfile=os.environ.get('MANAGER_TLS_CERT_FILE', '/etc/execution-manager/tls/tls.crt'),
          keyfile=os.environ.get('MANAGER_TLS_KEY_FILE', '/etc/execution-manager/tls/tls.key'))
