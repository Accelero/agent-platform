"""Authenticated scope API; trusted clients select named templates and optional images.

Transport-independent: `dispatch` takes an authenticated client and a decoded
body and returns the response value or raises a typed error.
"""
from __future__ import annotations

import hashlib
import hmac
import json
import re

from .domain.errors import Invalid, NotFound
from .service import Service

IDENTIFIER = re.compile(r'[A-Za-z0-9][A-Za-z0-9_.:-]{0,255}')
POST_FIELDS = {
    '/v1/leases/acquire': ({'profile', 'session', 'mode', 'request'},
                           {'runtime', 'parent_lease', 'image', 'template'}),
    '/v1/leases/heartbeat': ({'lease'}, set()),
    '/v1/leases/release': ({'lease'}, set()),
    '/v1/sessions/close': ({'profile', 'session'}, {'runtime'}),
}
GET_PATHS = ('/v1/environments', '/v1/orphans')


def identifier(value) -> str:
    if not isinstance(value, str) or not IDENTIFIER.fullmatch(value):
        raise Invalid('Invalid scope identifier')
    return value


def strict_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise Invalid('Duplicate JSON field')
        result[key] = value
    return result


def decode(body: bytes):
    return json.loads(body, object_pairs_hook=strict_object)


class ExecutionAPI:
    def __init__(self, service: Service, credentials: dict[str, str]):
        self.service = service
        config = service.config
        self.clients = config['clients']
        if not self.clients or set(credentials) != set(self.clients):
            raise ValueError('Every configured client needs a separate credential')
        if any(not isinstance(token, str) or len(token) < 32 for token in credentials.values()):
            raise ValueError('Client credentials must have at least 32 characters')
        if len(set(credentials.values())) != len(credentials):
            raise ValueError('Client credentials must be distinct')
        if config.get('default_template', 'hermes') not in config['templates']:
            raise ValueError('Default terminal template is not configured')
        self.tokens = {name: hashlib.sha256(token.encode()).digest()
                       for name, token in credentials.items()}
        deployments = []
        for client in self.clients.values():
            deployment = client['deployment']
            if deployment not in config.get('deployments', {}):
                raise ValueError('Client deployment is not configured')
            deployments.append(deployment)
        if len(set(deployments)) != len(deployments):
            raise ValueError('Each client must have its own deployment scope')

    def authenticate(self, authorization: str) -> str | None:
        if not authorization.startswith('Bearer '):
            return None
        digest = hashlib.sha256(authorization[7:].encode()).digest()
        for name, expected in self.tokens.items():
            if hmac.compare_digest(digest, expected):
                return name
        return None

    def dispatch(self, client: str, path: str, payload=None):
        deployment = self.clients[client]['deployment']
        if payload is None:
            if path == '/v1/environments':
                return self.service.status(deployment=deployment)
            if path == '/v1/orphans':
                return self.service.orphans()
            raise NotFound('Not found')
        if path not in POST_FIELDS:
            raise NotFound('Not found')
        required, optional = POST_FIELDS[path]
        if not isinstance(payload, dict) or not required <= payload.keys() or payload.keys() - required - optional:
            raise Invalid('Invalid request fields')
        if 'lease' in payload:
            lease = identifier(payload['lease'])
            if path.endswith('/heartbeat'):
                return self.service.heartbeat(lease, deployment=deployment)
            return self.service.release(lease, deployment=deployment, reconcile=False)
        profile, session = identifier(payload['profile']), identifier(payload['session'])
        if path == '/v1/sessions/close':
            return self.service.close_session(deployment, profile, session,
                                              runtime=payload.get('runtime'), reconcile=False)
        request = identifier(payload['request'])
        if 'parent_lease' in payload:
            identifier(payload['parent_lease'])
        template = identifier(payload.get('template', self.service.catalog.default))
        # Idempotency keys are local to the authenticated client.
        scoped = hashlib.sha256(json.dumps([deployment, request]).encode()).hexdigest()
        return self.service.acquire(dict(payload, deployment=deployment, template=template,
                                         request=scoped))
