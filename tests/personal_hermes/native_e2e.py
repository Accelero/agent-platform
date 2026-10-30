"""End to end on a disposable k3s server: the chart's manager and gateway,
real Hermes instances, the repository's terminal backend image and a Keycloak
realm, driven only through the administration API and the Hermes hostname.

`kubernetes.py --native` runs it after the integration tests. Run it alone
with `python -m tests.personal_hermes.native_e2e --postgres-container <name>`;
`--keep` leaves the cluster and the database for inspection after a failure;
a successful run always removes everything it made.

Every credential (TLS keys, sealing keys, session keys, passwords) is generated
for the run and stays in its temporary directory. Nothing secret is printed.
"""

import argparse
import base64
import hashlib
import io
import itertools
import json
import os
import re
import secrets
import socket
import ssl
import subprocess
import tarfile
import tempfile
import threading
import time
import uuid
from contextlib import contextmanager
from html import unescape
from pathlib import Path
from types import SimpleNamespace

import requests
import websocket
import yaml
from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey
from cryptography.hazmat.primitives.serialization import (
    Encoding,
    NoEncryption,
    PrivateFormat,
    PublicFormat,
)
from requests.adapters import HTTPAdapter
from urllib3.util.retry import Retry

from tests.personal_hermes.kubernetes import (
    ROOT,
    WORKER_TAG,
    command,
    import_image,
    import_worker_image,
    remove_container,
    start_k3s,
    wait_ready,
)
from tests.support.ownership import (
    FIXTURE,
    KUBERNETES,
    POSTGRES,
    REGISTRY_FIXTURE,
    RUN,
    Cleanup,
    database_comment,
    new_run,
    remove_fixture,
    require_fixture,
    run_databases,
)

NAMESPACE = "agents"
PUBLIC = "hermes.e2e.test"
ADMIN = "hermes-admin.e2e.test"
KEYCLOAK = "keycloak.keycloak.svc.cluster.local"
ISSUER = f"https://{KEYCLOAK}:8443/realms/agents"
GATEWAY_IDENTITY = "spiffe://e2e.test/hermes-gateway"
KEYCLOAK_IMAGE = (
    "quay.io/keycloak/keycloak@sha256:"
    "27b3230fbabb8c9ff3c66c864cf09dcd8a41aca82e5100679d109ef0eeb05741"
)
MANAGER_IMAGE = "hermes-manager:e2e"
HERMES_IMAGE = "personal-hermes-native-test:local"
NODE_PORTS = {"keycloak": 30843, "gateway": 30443, "admin": 30444}
# Exact Keycloak subjects, fixed in the imported realm.
SUBJECTS = {
    "alice": "00000000-0000-4000-8000-0000000000a1",
    "bob": "00000000-0000-4000-8000-0000000000b0",
    "carol": "00000000-0000-4000-8000-0000000000c0",
    "mallory": "00000000-0000-4000-8000-0000000000d0",
    "erin": "00000000-0000-4000-8000-0000000000e0",
}
READY_TIMEOUT = 900
HERE = Path(__file__).resolve().parent
# The in-cluster fake model (fake_model.py) every assistant instance uses.
MODEL_URL = "http://fake-model.model.svc.cluster.local:8080/v1"
REGISTRY = "registry.e2e.test:5000"
REGISTRY_IMAGE = (
    "registry@sha256:ddf754342cfc8acc51a56d5d0ab6af06826461864460636d8bd5c546dab2a7b8"
)


def kubectl(identifier, *args, data=None, timeout=120, check=True, quiet=False):
    """kubectl inside the fixture cluster. A failure shows kubectl's error,
    except with `quiet`, for commands whose input or output holds secrets."""
    result = subprocess.run(
        ["docker", "exec", "-i", identifier, "kubectl", *args],
        input=data,
        capture_output=True,
        timeout=timeout,
    )
    if check and result.returncode != 0:
        detail = "" if quiet else ": " + result.stderr.decode(errors="replace")[:800]
        raise AssertionError(f"kubectl {args[0]} failed{detail}")
    return result.stdout.decode()


def apply(identifier, documents):
    has_secret = any(document.get("kind") == "Secret" for document in documents)
    kubectl(
        identifier,
        "apply",
        "-f",
        "-",
        data=yaml.safe_dump_all(documents).encode(),
        quiet=has_secret,
    )


def b64(value):
    return base64.b64encode(
        value if isinstance(value, bytes) else value.encode()
    ).decode()


def secret(name, namespace, files):
    return {
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {"name": name, "namespace": namespace},
        "data": {key: b64(value) for key, value in files.items()},
    }


def openssl(*args):
    command(["openssl", *args], timeout=60)


def certificates(directory):
    """A throwaway CA and the fixture's server and client certificates."""
    path = Path(directory)
    openssl(
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        str(path / "ca.key"),
        "-out",
        str(path / "ca.crt"),
        "-days",
        "2",
        "-subj",
        "/CN=hermes-e2e-ca",
        "-addext",
        "basicConstraints=critical,CA:TRUE",
        "-addext",
        "keyUsage=critical,keyCertSign,cRLSign",
    )
    leaves = {
        "api": (
            f"DNS:hermes-manager-api.{NAMESPACE}.svc,DNS:{ADMIN}",
            "serverAuth",
        ),
        "gateway": (f"DNS:{PUBLIC},DNS:hermes-gateway.{NAMESPACE}.svc", "serverAuth"),
        "gateway-client": (f"URI:{GATEWAY_IDENTITY}", "clientAuth"),
        "keycloak": (f"DNS:{KEYCLOAK},DNS:keycloak.keycloak.svc", "serverAuth"),
    }
    for serial, (name, (san, usage)) in enumerate(leaves.items(), start=2):
        (path / f"{name}.ext").write_text(
            "basicConstraints=critical,CA:FALSE\n"
            f"subjectAltName={san}\n"
            f"extendedKeyUsage={usage}\n"
            "keyUsage=digitalSignature,keyEncipherment\n"
            "subjectKeyIdentifier=hash\n"
            "authorityKeyIdentifier=keyid,issuer\n"
        )
        openssl(
            "req",
            "-new",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            str(path / f"{name}.key"),
            "-out",
            str(path / f"{name}.csr"),
            "-subj",
            f"/CN={name}",
        )
        openssl(
            "x509",
            "-req",
            "-in",
            str(path / f"{name}.csr"),
            "-CA",
            str(path / "ca.crt"),
            "-CAkey",
            str(path / "ca.key"),
            "-set_serial",
            str(serial),
            "-out",
            str(path / f"{name}.crt"),
            "-days",
            "2",
            "-extfile",
            str(path / f"{name}.ext"),
        )
    return {
        name: (path / name).read_bytes()
        for name in [p.name for p in path.iterdir() if p.suffix in (".crt", ".key")]
    }


def realm(passwords):
    """The `agents` realm: the fixture people, the three OIDC clients and a
    service account that manages users, standing in for the provider's own
    administration."""

    def audience(name):
        return {
            "name": f"audience-{name}",
            "protocol": "openid-connect",
            "protocolMapper": "oidc-audience-mapper",
            "config": {
                "included.custom.audience": name,
                "access.token.claim": "true",
                "id.token.claim": "false",
            },
        }

    def client(client_id, callback, audience_name, password_grant=False):
        return {
            "clientId": client_id,
            "enabled": True,
            "publicClient": True,
            "standardFlowEnabled": True,
            "directAccessGrantsEnabled": password_grant,
            "redirectUris": [callback],
            "webOrigins": ["+"],
            "attributes": {"pkce.code.challenge.method": "S256"},
            "protocolMappers": [audience(audience_name)],
        }

    return {
        "realm": "agents",
        "enabled": True,
        "sslRequired": "all",
        "users": [
            {
                "id": subject,
                "username": name,
                "enabled": True,
                "email": f"{name}@e2e.test",
                "emailVerified": True,
                "firstName": name.title(),
                "lastName": "Fixture",
                "credentials": [
                    {"type": "password", "value": passwords[name], "temporary": False}
                ],
            }
            for name, subject in SUBJECTS.items()
        ]
        + [
            {
                "username": "service-account-e2e-directory",
                "enabled": True,
                "serviceAccountClientId": "e2e-directory",
                "clientRoles": {
                    "realm-management": ["manage-users", "view-users", "manage-realm"]
                },
            }
        ],
        "clients": [
            {
                "clientId": "e2e-directory",
                "enabled": True,
                "publicClient": False,
                "secret": passwords["directory"],
                "serviceAccountsEnabled": True,
                "standardFlowEnabled": False,
                "directAccessGrantsEnabled": False,
            },
            client(
                "manager-admin",
                f"https://{ADMIN}/auth/oidc/callback",
                "hermes-manager",
                password_grant=True,
            ),
            client(
                "hermes-gateway",
                f"https://{PUBLIC}/_platform/oidc/callback",
                "hermes-gateway",
            ),
            client("hermes-native", f"https://{PUBLIC}/auth/callback", "hermes-native"),
        ],
    }


def keycloak(identifier, pinned, tls, passwords):
    labels = {"app": "keycloak"}
    apply(
        identifier,
        [
            {"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "keycloak"}},
            secret(
                "keycloak-tls",
                "keycloak",
                {"tls.crt": tls["keycloak.crt"], "tls.key": tls["keycloak.key"]},
            ),
            {
                "apiVersion": "v1",
                "kind": "ConfigMap",
                "metadata": {"name": "keycloak-realm", "namespace": "keycloak"},
                "data": {"agents.json": json.dumps(realm(passwords))},
            },
            {
                "apiVersion": "apps/v1",
                "kind": "Deployment",
                "metadata": {"name": "keycloak", "namespace": "keycloak"},
                "spec": {
                    "replicas": 1,
                    "selector": {"matchLabels": labels},
                    "template": {
                        "metadata": {"labels": labels},
                        "spec": {
                            "containers": [
                                {
                                    "name": "keycloak",
                                    "image": pinned,
                                    "args": [
                                        "start-dev",
                                        "--import-realm",
                                        "--http-enabled=false",
                                        "--https-port=8443",
                                        "--https-certificate-file=/etc/keycloak/tls/tls.crt",
                                        "--https-certificate-key-file=/etc/keycloak/tls/tls.key",
                                        f"--hostname=https://{KEYCLOAK}:8443",
                                    ],
                                    "ports": [{"containerPort": 8443}],
                                    "readinessProbe": {
                                        "tcpSocket": {"port": 8443},
                                        "periodSeconds": 3,
                                    },
                                    "resources": {"limits": {"memory": "1536Mi"}},
                                    "volumeMounts": [
                                        {
                                            "name": "tls",
                                            "mountPath": "/etc/keycloak/tls",
                                        },
                                        {
                                            "name": "realm",
                                            "mountPath": "/opt/keycloak/data/import",
                                        },
                                    ],
                                }
                            ],
                            "volumes": [
                                {
                                    "name": "tls",
                                    "secret": {"secretName": "keycloak-tls"},
                                },
                                {
                                    "name": "realm",
                                    "configMap": {"name": "keycloak-realm"},
                                },
                            ],
                        },
                    },
                },
            },
            node_port_service(
                "keycloak", "keycloak", labels, 8443, NODE_PORTS["keycloak"]
            ),
        ],
    )


def fake_model(identifier, image):
    """The OpenAI-compatible fake model in namespace `model`, run by the
    Hermes image's Python."""
    labels = {"app": "fake-model"}
    apply(
        identifier,
        [
            {"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "model"}},
            {
                "apiVersion": "v1",
                "kind": "ConfigMap",
                "metadata": {"name": "fake-model", "namespace": "model"},
                "data": {"fake_model.py": (HERE / "fake_model.py").read_text()},
            },
            {
                "apiVersion": "apps/v1",
                "kind": "Deployment",
                "metadata": {"name": "fake-model", "namespace": "model"},
                "spec": {
                    "replicas": 1,
                    "selector": {"matchLabels": labels},
                    "template": {
                        "metadata": {"labels": labels},
                        "spec": {
                            "securityContext": {
                                "runAsNonRoot": True,
                                "runAsUser": 1000,
                                "runAsGroup": 1000,
                            },
                            "containers": [
                                {
                                    "name": "model",
                                    "image": image,
                                    "command": [
                                        "/opt/hermes/.venv/bin/python",
                                        "/fake/fake_model.py",
                                    ],
                                    "ports": [{"containerPort": 8080}],
                                    "readinessProbe": {
                                        "tcpSocket": {"port": 8080},
                                        "periodSeconds": 2,
                                    },
                                    "volumeMounts": [
                                        {"name": "fake", "mountPath": "/fake"}
                                    ],
                                }
                            ],
                            "volumes": [
                                {"name": "fake", "configMap": {"name": "fake-model"}}
                            ],
                        },
                    },
                },
            },
            {
                "apiVersion": "v1",
                "kind": "Service",
                "metadata": {"name": "fake-model", "namespace": "model"},
                "spec": {"selector": labels, "ports": [{"port": 8080}]},
            },
        ],
    )


def model_requests(identifier):
    """What the fake model saw: `{path, key, model}` per request, keys
    hashed."""
    output = kubectl(
        identifier,
        "exec",
        "--namespace",
        "model",
        "deployment/fake-model",
        "--",
        "/opt/hermes/.venv/bin/python",
        "-c",
        "import sys, urllib.request; "
        "sys.stdout.write(urllib.request.urlopen('http://127.0.0.1:8080/seen').read().decode())",
    )
    return json.loads(output)["requests"]


def node_port_service(name, namespace, selector, port, node_port):
    return {
        "apiVersion": "v1",
        "kind": "Service",
        "metadata": {"name": name, "namespace": namespace},
        "spec": {
            "type": "NodePort",
            "selector": selector,
            "ports": [{"port": port, "targetPort": port, "nodePort": node_port}],
        },
    }


@contextmanager
def routed(node_ip):
    """Resolves the fixture's hostnames to their node ports, for this process."""
    targets = {
        (KEYCLOAK, 8443): NODE_PORTS["keycloak"],
        (PUBLIC, 443): NODE_PORTS["gateway"],
        (ADMIN, 443): NODE_PORTS["admin"],
    }
    original = socket.getaddrinfo

    def getaddrinfo(host, port, *args, **kwargs):
        target = targets.get((host, int(port) if port is not None else None))
        if target is None:
            return original(host, port, *args, **kwargs)
        return [
            (
                socket.AF_INET,
                socket.SOCK_STREAM,
                socket.IPPROTO_TCP,
                "",
                (node_ip, target),
            )
        ]

    socket.getaddrinfo = getaddrinfo
    try:
        yield
    finally:
        socket.getaddrinfo = original


def wait(condition, timeout, message, interval=3.0):
    """Polls until `condition` is truthy; the failure names the last error type."""
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            value = condition()
        except (requests.RequestException, OSError) as error:
            value, last = None, type(error).__name__
        if value:
            return value
        time.sleep(interval)
    text = message() if callable(message) else message
    raise AssertionError(f"{text} (last error: {last})" if last else text)


def container_ip(container, network=None):
    networks = json.loads(command(["docker", "inspect", container]))[0][
        "NetworkSettings"
    ]["Networks"]
    if network is not None:
        return networks[network]["IPAddress"]
    return next(iter(networks.values()))["IPAddress"]


def shared_network(first, second):
    def names(container):
        return set(
            json.loads(command(["docker", "inspect", container]))[0]["NetworkSettings"][
                "Networks"
            ]
        )

    shared = sorted(names(first) & names(second))
    assert shared, "PostgreSQL and the cluster share no network"
    return shared[0]


def resilient(session, methods=Retry.DEFAULT_ALLOWED_METHODS):
    """Retries a request on a kept-alive connection the server has just
    closed, as browsers do; only for `methods`."""
    retry = Retry(total=2, connect=2, read=2, status=0, allowed_methods=methods)
    session.mount("https://", HTTPAdapter(max_retries=retry))
    return session


class Admin:
    """The administration API as Alice, with a bearer token."""

    def __init__(self, ca, password):
        # Every command carries its idempotency key, so a resent one is safe.
        self.session = resilient(requests.Session(), methods=None)
        self.session.verify = ca
        self.password = password
        self.token = None
        self.issued = 0.0

    def headers(self, etag=None):
        if time.monotonic() - self.issued > 60:
            reply = self.session.post(
                f"{ISSUER}/protocol/openid-connect/token",
                data={
                    "grant_type": "password",
                    "client_id": "manager-admin",
                    "username": "alice",
                    "password": self.password,
                    "scope": "openid",
                },
                timeout=20,
            )
            assert reply.status_code == 200, "Keycloak refused the administrator"
            self.token = reply.json()["access_token"]
            self.issued = time.monotonic()
        headers = {
            "Authorization": f"Bearer {self.token}",
            "Idempotency-Key": uuid.uuid4().hex,
        }
        if etag is not None:
            headers["If-Match"] = etag
        return headers

    def call(self, method, path, body=None, etag=None, expected=(200, 201, 202)):
        reply = self.session.request(
            method,
            f"https://{ADMIN}{path}",
            json=body,
            headers=self.headers(etag),
            timeout=30,
        )
        assert reply.status_code in expected, (
            f"{method} {path}: {reply.status_code} {reply.text[:500]}"
        )
        return reply

    def publish(self, kind, catalog_id, definition):
        draft = self.call(
            "POST", f"/api/v1/{kind}/{catalog_id}/versions", {"definition": definition}
        ).json()
        self.call(
            "POST",
            f"/api/v1/{kind}/{catalog_id}/versions/{draft['version']}/publish",
            etag=f'"{draft["revision"]}"',
        )
        return draft["version"]

    def assignment(self, assignment_id):
        reply = self.call("GET", f"/api/v1/assignments/{assignment_id}")
        return reply.json(), reply.headers["ETag"]

    def set_access(self, assignment_id, access):
        _, etag = self.assignment(assignment_id)
        return self.call(
            "PATCH",
            f"/api/v1/assignments/{assignment_id}",
            {"access": access},
            etag=etag,
        ).json()

    def phase(self, instance_id):
        return self.call("GET", f"/api/v1/instances/{instance_id}").json()


def keycloak_login(session, page, username, password):
    """Submits Keycloak's login form from `page`; follows to the end."""
    match = re.search(r'<form[^>]*id="kc-form-login"[^>]*action="([^"]+)"', page.text)
    assert match, f"no Keycloak login form for {username}"
    return session.post(
        unescape(match.group(1)),
        data={"username": username, "password": password, "credentialId": ""},
        # As a browser submits a form; the redirects after it are page loads.
        headers={"Accept": "text/html,application/xhtml+xml"},
        timeout=30,
    )


class Browser:
    """One person's browser: platform session, native session, cookies."""

    def __init__(self, ca, name, password):
        self.session = resilient(requests.Session())
        self.session.verify = ca
        self.ca = ca
        self.name = name
        self.password = password

    def get(self, path, **kwargs):
        kwargs.setdefault("timeout", 30)
        return self.session.get(f"https://{PUBLIC}{path}", **kwargs)

    def post(self, path, **kwargs):
        kwargs.setdefault("timeout", 30)
        headers = {"Origin": f"https://{PUBLIC}", **kwargs.pop("headers", {})}
        return self.session.post(f"https://{PUBLIC}{path}", headers=headers, **kwargs)

    def sign_in(self):
        """Platform login from a protected page, through Keycloak."""
        page = self.get("/", headers={"Accept": "text/html"})
        assert KEYCLOAK in page.url, f"{self.name} was not sent to Keycloak"
        return keycloak_login(self.session, page, self.name, self.password)

    def native_sign_in(self):
        """Native Hermes's own OIDC login, which reuses the Keycloak session."""
        providers = self.get("/api/auth/providers").json()
        listed = providers.get("providers", providers)
        name = next(item["name"] if isinstance(item, dict) else item for item in listed)
        reply = self.get("/auth/login", params={"provider": name, "next": "/"})
        assert reply.status_code == 200, (
            f"native login for {self.name}: {reply.status_code}"
        )
        me = self.get("/api/auth/me")
        assert me.status_code == 200, (
            f"native session for {self.name}: {me.status_code}"
        )
        return me.json()

    def cookie_header(self):
        return "; ".join(
            f"{cookie.name}={cookie.value}"
            for cookie in self.session.cookies
            if cookie.domain.lstrip(".") == PUBLIC
        )

    def websocket(self):
        ticket = self.post("/api/auth/ws-ticket")
        assert ticket.status_code == 200, f"ws ticket: {ticket.status_code}"
        connection = websocket.create_connection(
            f"wss://{PUBLIC}/api/events?channel=e2e&ticket={ticket.json()['ticket']}",
            header=[f"Cookie: {self.cookie_header()}"],
            origin=f"https://{PUBLIC}",
            sslopt={"ca_certs": self.ca, "cert_reqs": ssl.CERT_REQUIRED},
            timeout=20,
        )
        return connection

    def put(self, path, **kwargs):
        kwargs.setdefault("timeout", 30)
        headers = {"Origin": f"https://{PUBLIC}", **kwargs.pop("headers", {})}
        return self.session.put(f"https://{PUBLIC}{path}", headers=headers, **kwargs)

    def upload(self, path, data):
        """Native Hermes's managed file upload."""
        encoded = base64.b64encode(data).decode()
        return self.post(
            "/api/files/upload",
            json={
                "path": path,
                "data_url": f"data:text/plain;base64,{encoded}",
                "overwrite": True,
            },
        )

    def rpc(self):
        """Native chat's JSON-RPC WebSocket, through the gateway."""
        ticket = self.post("/api/auth/ws-ticket")
        assert ticket.status_code == 200, f"ws ticket: {ticket.status_code}"
        return websocket.create_connection(
            f"wss://{PUBLIC}/api/ws?ticket={ticket.json()['ticket']}",
            header=[f"Cookie: {self.cookie_header()}"],
            origin=f"https://{PUBLIC}",
            sslopt={"ca_certs": self.ca, "cert_reqs": ssl.CERT_REQUIRED},
            timeout=60,
        )

    def chat(self, text, timeout=240):
        """One turn in a new native session; returns the session and the
        turn's final text."""
        connection = self.rpc()
        try:
            return converse(connection, text, timeout)
        finally:
            connection.close()


def converse(connection, text, timeout):
    """`session.create`, then `prompt.submit`, then the session's
    `message.complete` event, over newline-delimited JSON-RPC."""
    deadline = time.monotonic() + timeout
    held = []

    def frames():
        while True:
            connection.settimeout(max(1.0, deadline - time.monotonic()))
            for line in connection.recv().splitlines():
                if line.strip():
                    yield json.loads(line)

    stream = frames()

    def call(ident, method, params):
        connection.send(
            json.dumps(
                {"jsonrpc": "2.0", "id": ident, "method": method, "params": params}
            )
        )
        for frame in stream:
            if frame.get("id") == ident:
                assert "error" not in frame, f"{method}: {frame['error']}"
                return frame["result"]
            held.append(frame)
        raise AssertionError(f"{method}: no reply")

    session = call(1, "session.create", {})
    call(2, "prompt.submit", {"session_id": session["session_id"], "text": text})
    for frame in itertools.chain(held, stream):
        params = frame.get("params") or {}
        if (
            frame.get("method") == "event"
            and params.get("type") == "message.complete"
            and params.get("session_id") == session["session_id"]
        ):
            return session, str((params.get("payload") or {}).get("text", ""))
    raise AssertionError("the turn never completed")


class Directory:
    """The provider's user administration (Keycloak's admin API), as its own
    administrator would change people."""

    def __init__(self, ca, secret):
        self.session = requests.Session()
        self.session.verify = ca
        self.secret = secret

    def call(self, method, path, body=None, expected=(200, 201, 204)):
        token = self.session.post(
            f"{ISSUER}/protocol/openid-connect/token",
            data={
                "grant_type": "client_credentials",
                "client_id": "e2e-directory",
                "client_secret": self.secret,
            },
            timeout=20,
        )
        assert token.status_code == 200, "Keycloak refused the directory client"
        reply = self.session.request(
            method,
            f"https://{KEYCLOAK}:8443/admin/realms/agents{path}",
            json=body,
            headers={"Authorization": f"Bearer {token.json()['access_token']}"},
            timeout=20,
        )
        assert reply.status_code in expected, f"{method} {path}: {reply.status_code}"
        return reply

    def update(self, subject, **changes):
        user = self.call("GET", f"/users/{subject}").json()
        user.update(changes)
        self.call("PUT", f"/users/{subject}", user)

    def create(self, username, email, password):
        """A new person; returns their new subject."""
        reply = self.call(
            "POST",
            "/users",
            {
                "username": username,
                "email": email,
                "emailVerified": True,
                "enabled": True,
                "firstName": username.title(),
                "lastName": "Fixture",
                "credentials": [
                    {"type": "password", "value": password, "temporary": False}
                ],
            },
            expected=(201,),
        )
        return reply.headers["Location"].rsplit("/", 1)[1]


def closed_within(connection, seconds):
    """Seconds until the server closes `connection`."""
    started = time.monotonic()
    connection.settimeout(seconds)
    try:
        while True:
            frame = connection.recv()
            if frame in ("", b"", None):
                break
    except websocket.WebSocketException, OSError:
        pass
    return time.monotonic() - started


def installation():
    """The installation's ID and the controller's sealing key pair. A restore
    keeps both: materials and admin secrets are sealed to the key."""
    sealing = X25519PrivateKey.generate()
    return {
        "id": str(uuid.uuid4()),
        "private": sealing.private_bytes(
            Encoding.Raw, PrivateFormat.Raw, NoEncryption()
        ),
        "public": sealing.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw),
    }


FRESH = [
    ("migrate", "hermes-manager-migrate"),
    ("initialize", "hermes-manager-initialize"),
    ("grantAdministrator", "hermes-manager-grant-administrator"),
]
RESTORED = [
    ("migrate", "hermes-manager-migrate"),
    ("rebindCluster", "hermes-manager-rebind-cluster"),
]


def install(
    identifier, kubeconfig, temporary, images, database_url, tls, node, owned, steps
):
    """Secrets, then the chart with each operator step, then the roles."""
    private, public = owned["private"], owned["public"]
    identity = json.dumps({"issuer": ISSUER, "subject": SUBJECTS["alice"]})
    apply(
        identifier,
        [
            {"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": NAMESPACE}},
            secret(
                "hermes-manager-api",
                NAMESPACE,
                {
                    "database-url": database_url,
                    "admin-tls.crt": tls["api.crt"],
                    "admin-tls.key": tls["api.key"],
                    "execution-tls.crt": tls["api.crt"],
                    "execution-tls.key": tls["api.key"],
                    "resolver-tls.crt": tls["api.crt"],
                    "resolver-tls.key": tls["api.key"],
                    "resolver-client-ca.crt": tls["ca.crt"],
                    "secret-sealing.pub": public,
                    "session-database-url": database_url,
                    "session-key-k1": secrets.token_bytes(32),
                    "oidc-ca.crt": tls["ca.crt"],
                },
            ),
            secret(
                "hermes-manager-controller",
                NAMESPACE,
                {
                    "database-url": database_url,
                    "secret-sealing.key": private,
                    "execution-ca.crt": tls["ca.crt"],
                    "oidc-ca.crt": tls["ca.crt"],
                },
            ),
            secret(
                "hermes-gateway",
                NAMESPACE,
                {
                    "database-url": database_url,
                    "tls.crt": tls["gateway.crt"],
                    "tls.key": tls["gateway.key"],
                    "session-key-g1": secrets.token_bytes(32),
                    "resolver-ca.crt": tls["ca.crt"],
                    "resolver-client.crt": tls["gateway-client.crt"],
                    "resolver-client.key": tls["gateway-client.key"],
                    "oidc-ca.crt": tls["ca.crt"],
                },
            ),
            secret("hermes-manager-schema", NAMESPACE, {"database-url": database_url}),
            secret(
                "hermes-first-administrator",
                NAMESPACE,
                {"database-url": database_url, "identity.json": identity},
            ),
        ],
    )
    jwks = f"{ISSUER}/protocol/openid-connect/certs"
    everyone = [{"ipBlock": {"cidr": "0.0.0.0/0"}}]
    values = {
        "images": {"hermes-manager": images["manager"]},
        "hermesManager": {
            "enabled": True,
            "installation": {"id": owned["id"], "issuer": ISSUER},
            "storageSources": [
                {"id": "local", "storageClass": {"name": "local-path"}},
                {"id": "host", "localPath": {"node": node, "root": "/srv/hermes-data"}},
            ],
            "publicOrigin": f"https://{PUBLIC}",
            "adminOrigin": f"https://{ADMIN}",
            "oidcPrivateCa": True,
            "publicHttps": False,
            "serviceEgress": [
                {"namespace": "keycloak", "labels": {"app": "keycloak"}, "port": 8443},
                {"namespace": "model", "labels": {"app": "fake-model"}, "port": 8080},
            ],
            "api": {
                "existingSecret": "hermes-manager-api",
                "tokens": {
                    "jwksUrl": jwks,
                    "audience": "hermes-manager",
                    "authorizedClients": ["manager-admin"],
                },
                "browser": {"sessionKeys": {"active": "k1", "ids": ["k1"]}},
                "gatewayIdentity": GATEWAY_IDENTITY,
            },
            "controller": {"existingSecret": "hermes-manager-controller"},
            "gateway": {
                "existingSecret": "hermes-gateway",
                "oidc": {
                    "clientId": "hermes-gateway",
                    "jwksUrl": jwks,
                    "audience": "hermes-gateway",
                },
                "sessionKeys": {"active": "g1", "ids": ["g1"]},
            },
            "ingress": {"admin": everyone, "gateway": everyone},
        },
    }

    def helm(operations):
        values["hermesManager"]["operations"] = operations
        path = Path(temporary) / "values.yaml"
        path.write_text(yaml.safe_dump(values))
        command(
            [
                "helm",
                "upgrade",
                "--install",
                "agents",
                str(ROOT / "charts/agent-platform"),
                "--namespace",
                NAMESPACE,
                "--kubeconfig",
                str(kubeconfig),
                "--values",
                str(path),
            ],
            timeout=120,
        )

    off = {
        "migrate": {"enabled": False, "existingSecret": "hermes-manager-schema"},
        "initialize": {"enabled": False},
        "rebindCluster": {"enabled": False},
        "grantAdministrator": {
            "enabled": False,
            "existingSecret": "hermes-first-administrator",
            "recover": False,
        },
    }
    for step, job in steps:
        operations = json.loads(json.dumps(off))
        operations[step]["enabled"] = True
        helm(operations)
        revision = json.loads(
            command(
                [
                    "helm",
                    "history",
                    "agents",
                    "--namespace",
                    NAMESPACE,
                    "--kubeconfig",
                    str(kubeconfig),
                    "--output",
                    "json",
                ]
            )
        )[-1]["revision"]
        kubectl(
            identifier,
            "wait",
            "--namespace",
            NAMESPACE,
            "--for=condition=complete",
            f"job/{job}-{revision}",
            "--timeout=300s",
            timeout=320,
        )
    helm(off)
    for name in ("hermes-manager-api", "hermes-manager-controller", "hermes-gateway"):
        kubectl(
            identifier,
            "rollout",
            "status",
            "--namespace",
            NAMESPACE,
            f"deployment/{name}",
            "--timeout=300s",
            timeout=320,
        )
    apply(
        identifier,
        [
            node_port_service(
                "hermes-gateway-e2e",
                NAMESPACE,
                {"app.kubernetes.io/name": "hermes-gateway"},
                8443,
                NODE_PORTS["gateway"],
            ),
            node_port_service(
                "hermes-manager-admin-e2e",
                NAMESPACE,
                {"app.kubernetes.io/name": "hermes-manager-api"},
                8443,
                NODE_PORTS["admin"],
            ),
        ],
    )


def instance_pod_exec(identifier, namespace, script):
    return kubectl(
        identifier,
        "exec",
        "--namespace",
        namespace,
        "deployment/hermes",
        "-c",
        "hermes",
        "--",
        "sh",
        "-c",
        script,
    )


def run_e2e(identifier, temporary, environment):
    """The acceptance scenarios on an existing disposable cluster."""
    postgres = environment["HERMES_TEST_POSTGRES_CONTAINER"]
    kubeconfig = environment["HERMES_TEST_KUBECONFIG"]
    reuse = bool(environment.get("HERMES_TEST_REUSE"))
    worker = environment.get("HERMES_TEST_WORKER_IMAGE")
    if not worker:
        worker = (
            import_image(identifier, WORKER_TAG, temporary, reuse)
            if reuse
            else import_worker_image(identifier, temporary)
        )
    images = {
        "manager": import_image(identifier, MANAGER_IMAGE, temporary, reuse),
        "hermes": import_image(identifier, HERMES_IMAGE, temporary, reuse),
        "worker": worker,
    }
    command(["docker", "pull", "--quiet", KEYCLOAK_IMAGE], timeout=900)
    command(["docker", "tag", KEYCLOAK_IMAGE, "hermes-e2e-keycloak:pinned"])
    keycloak_image = import_image(
        identifier, "hermes-e2e-keycloak:pinned", temporary, reuse
    )

    run = environment["HERMES_TEST_RUN"]
    database = "hermes_e2e_" + uuid.uuid4().hex[:8]

    def psql(statement):
        command(
            [
                "docker",
                "exec",
                postgres,
                "psql",
                "-U",
                "hermes_test",
                "-d",
                "hermes_test",
                "-c",
                statement,
            ]
        )

    with Cleanup() as cleanup:
        # The database belongs to this run by its comment, not by its name.
        cleanup.add(
            "run database",
            lambda: (
                print(f"Kept the database {database} for inspection.")
                if cleanup.failed and environment.get("HERMES_TEST_KEEP")
                else psql(f"DROP DATABASE IF EXISTS {database} WITH (FORCE)")
            ),
        )
        psql(f"CREATE DATABASE {database}")
        psql(f"COMMENT ON DATABASE {database} IS '{database_comment(run)}'")
        e2e_on_database(
            identifier,
            temporary,
            environment,
            images,
            keycloak_image,
            postgres,
            database,
            kubeconfig,
        )


def e2e_on_database(
    identifier,
    temporary,
    environment,
    images,
    keycloak_image,
    postgres,
    database,
    kubeconfig,
):
    network = shared_network(postgres, identifier)
    database_url = (
        f"postgres://hermes_test@{container_ip(postgres, network)}:5432/{database}"
    )
    node_ip = container_ip(identifier, network)
    node = kubectl(
        identifier, "get", "nodes", "-o", "jsonpath={.items[0].metadata.name}"
    )
    command(
        [
            "docker",
            "exec",
            identifier,
            "sh",
            "-c",
            "mkdir -p /srv/hermes-data/carol && chown 1000:1000 /srv/hermes-data/carol",
        ]
    )

    tls_directory = Path(temporary) / "tls"
    tls_directory.mkdir()
    tls = certificates(tls_directory)
    ca = str(tls_directory / "ca.crt")
    passwords = {name: secrets.token_urlsafe(18) for name in SUBJECTS}
    # Fixture secrets of this run: the directory client, the model key and
    # the private registry. None of them is ever printed.
    for name in ("directory", "llm", "registry"):
        passwords[name] = secrets.token_urlsafe(24)

    keycloak(identifier, keycloak_image, tls, passwords)
    fake_model(identifier, images["hermes"])
    for namespace, name in (("keycloak", "keycloak"), ("model", "fake-model")):
        kubectl(
            identifier,
            "rollout",
            "status",
            "--namespace",
            namespace,
            f"deployment/{name}",
            "--timeout=300s",
            timeout=320,
        )
    with routed(node_ip):
        wait(
            lambda: (
                requests.get(
                    f"{ISSUER}/.well-known/openid-configuration", verify=ca, timeout=5
                ).status_code
                == 200
            ),
            300,
            "Keycloak did not serve the realm",
        )
        owned = installation()
        install(
            identifier,
            kubeconfig,
            temporary,
            images,
            database_url,
            tls,
            node,
            owned,
            FRESH,
        )
        run = environment["HERMES_TEST_RUN"]
        with private_registry(
            identifier, network, temporary, passwords["registry"], run
        ) as image:
            images["private"] = image
            assignments = published_versions_commission_ready_instances_from_the_api(
                identifier, images, ca, passwords, postgres
            )
        restore_on_a_new_cluster_rebinds_without_adoption(
            identifier,
            postgres,
            network,
            database,
            temporary,
            tls,
            ca,
            passwords,
            owned,
            assignments,
            run,
        )


def commission(identifier, images, ca, passwords):
    """Publishes the catalog and commissions Bob's and Carol's instances through
    the administration API, and waits until both are ready. Everything a
    scenario needs is on the returned state; nothing is torn down."""
    admin = Admin(ca, passwords["alice"])
    wait(
        lambda: (
            admin.session.get(f"https://{ADMIN}/healthz", timeout=5).status_code == 200
        ),
        120,
        "the administration API is unreachable",
    )
    # The model key: an admin secret only the assistant template references.
    admin.call(
        "PUT",
        "/api/v1/secrets/llm-key",
        {"kind": "env", "value": passwords["llm"]},
    )
    base = {
        "schemaVersion": 2,
        "image": images["hermes"],
        "resources": {"cpuMillis": 1000, "memoryMib": 2048},
        "home": {"source": "local", "sizeGib": 1},
        "mounts": {"workspace": {"mountPath": "/workspace"}},
        "defaultVolumes": {"workspace": {"source": "local", "sizeGib": 1}},
    }
    template = admin.publish("templates", "personal", base)
    assistant = admin.publish(
        "templates",
        "assistant",
        {
            **base,
            "adminConfig": {
                "model": {
                    "provider": "custom",
                    "base_url": MODEL_URL,
                    "default": "fake-model",
                    "api_key": "${OPENAI_API_KEY}",
                }
            },
            "secrets": [{"name": "llm-key", "env": "OPENAI_API_KEY"}],
        },
    )
    worker = {
        "image": images["worker"],
        "sshContract": "managed-ssh-v2",
        "uid": 1000,
        "gid": 1000,
        "resources": {"cpuMillis": 500, "memoryMib": 512, "ephemeralGib": 1},
    }
    backend = admin.publish(
        "terminal-backends",
        "shell",
        {
            **worker,
            "limits": {
                "idleSeconds": 300,
                "maxLeaseSeconds": 900,
                "maxEnvironments": 4,
            },
            "mounts": {"workspace": {"mountPath": "/workspace"}},
        },
    )
    # Bob's second backend also mounts his network home and idles out fast.
    tools = admin.publish(
        "terminal-backends",
        "tools",
        {
            **worker,
            "limits": {"idleSeconds": 60, "maxLeaseSeconds": 900, "maxEnvironments": 2},
            "mounts": {
                "workspace": {"mountPath": "/workspace"},
                "home": {"mountPath": "/home/worker/home"},
            },
        },
    )
    command(
        [
            "docker",
            "exec",
            identifier,
            "sh",
            "-c",
            "mkdir -p /srv/hermes-data/homes/bob"
            " && chown -R 1000:1000 /srv/hermes-data/homes",
        ]
    )
    terminal = {"default": "shell", "allowed": {"shell": backend}}
    assignments = {}
    for name, spec in [
        (
            "bob",
            {
                "template": {"id": "assistant", "version": assistant},
                "volumes": {
                    "workspace": {"source": "local", "sizeGib": 1},
                    "home": {"source": "host", "path": "homes/bob"},
                },
                "terminal": {
                    "default": "shell",
                    "allowed": {"shell": backend, "tools": tools},
                },
                # Layered over the template's settings.
                "adminConfig": {"display": {"compact": True}},
            },
        ),
        (
            "carol",
            {
                "template": {"id": "personal", "version": template},
                "volumes": {"workspace": {"source": "host", "path": "carol"}},
                "terminal": terminal,
            },
        ),
    ]:
        assignments[name] = admin.call(
            "POST",
            "/api/v1/assignments",
            {
                "identity": {"issuer": ISSUER, "subject": SUBJECTS[name]},
                "spec": {"access": "enabled", **spec},
            },
        ).json()
    for name, receipt in assignments.items():
        wait(
            lambda receipt=receipt: (
                admin.phase(receipt["instanceId"])["phase"] == "ready"
            ),
            READY_TIMEOUT,
            lambda receipt=receipt, name=name: (
                f"{name}'s instance never became ready: "
                f"{admin.phase(receipt['instanceId'])}"
            ),
            interval=5,
        )
    versions = {"template": template, "shell": backend, "tools": tools}
    return SimpleNamespace(
        admin=admin,
        assignments=assignments,
        template=template,
        terminal=terminal,
        versions=versions,
        worker=worker,
    )


def published_versions_commission_ready_instances_from_the_api(
    identifier, images, ca, passwords, postgres
):
    state = commission(identifier, images, ca, passwords)
    admin, assignments = state.admin, state.assignments
    template, terminal = state.template, state.terminal
    versions, worker = state.versions, state.worker

    a_missing_static_directory_blocks_without_a_substitute(
        identifier, admin, template, terminal
    )
    owners_share_a_static_folder(identifier, admin, assignments, template, terminal)
    two_owners_reach_their_own_native_instances_through_the_gateway(
        identifier, admin, ca, passwords, assignments
    )
    a_hermes_without_browser_traffic_acquires_a_terminal_backend(
        identifier, admin, assignments
    )
    chat_settings_and_the_admin_secret_reach_only_the_referencing_instance(
        identifier, admin, ca, passwords, assignments
    )
    uploads_reach_only_the_owners_private_home(
        identifier, admin, ca, passwords, assignments
    )
    a_cron_job_fires_without_browser_traffic_and_acquires_a_terminal(
        identifier, admin, ca, passwords, assignments
    )
    bobs_backends_share_the_workspace_and_idle_workers_retire(
        identifier, admin, assignments
    )
    a_backend_identity_controls_its_workers_and_keeps_the_hermes_home_private(
        identifier, admin, template, worker
    )
    a_backend_change_keeps_active_leases_and_drains_superseded_environments(
        identifier, admin, assignments, worker, versions
    )
    a_private_backend_pulls_with_its_registry_secret_and_reports_failures(
        identifier, admin, assignments, images, passwords, worker
    )
    foreign_cookies_tickets_and_forged_headers_reach_nothing(ca, passwords)
    an_email_change_keeps_ownership_and_the_old_email_gets_nothing(
        ca, passwords, assignments
    )
    outages_and_provider_revocation_fail_closed_within_the_bounds(
        identifier, postgres, admin, ca, passwords, assignments
    )
    suspension_closes_open_connections_and_reenabling_keeps_data(
        identifier, admin, ca, passwords, assignments
    )
    only_the_controller_holds_kubernetes_credentials_and_logs_hold_no_secrets(
        identifier, admin, ca, passwords, assignments
    )
    print(
        "PASS: chart install with operator Jobs; two owners commissioned and reached "
        "through the gateway with Keycloak; owner-pinned native login; denial "
        "without assignment or enrollment; a missing static directory blocks, then "
        "recovers; static folders shared with independent read-only mounts; "
        "chat, settings precedence and the "
        "admin secret only where referenced; uploads isolated per owner; a cron "
        "job fired without browser traffic and acquired a terminal; a second "
        "backend with the private home only there; idle workers retired; a backend "
        "change kept the active lease; a private registry with pull failures "
        "reported; foreign cookies and tickets refused; an email change kept "
        "ownership; outages and provider revocation failed closed; suspension "
        "closed connections and drained a pending worker; data kept on re-enable; "
        "only the controller holds cluster rights; no secret in any log"
    )
    return assignments


def a_missing_static_directory_blocks_without_a_substitute(
    identifier, admin, template, terminal
):
    """A static volume whose directory does not exist blocks the instance with
    a storage condition; nothing creates the directory, and the instance
    starts once it exists."""
    receipt = admin.call(
        "POST",
        "/api/v1/assignments",
        {
            "identity": {"issuer": ISSUER, "subject": SUBJECTS["erin"]},
            "spec": {
                "access": "enabled",
                "template": {"id": "personal", "version": template},
                "volumes": {"workspace": {"source": "host", "path": "erin"}},
                "terminal": terminal,
            },
        },
    ).json()

    def blocked():
        view = admin.phase(receipt["instanceId"])
        storage = next((c for c in view["conditions"] if c["type"] == "storage"), None)
        return (
            view["phase"] == "blocked"
            and view["blockedReason"] == "storage_unavailable"
            and storage
            == {"type": "storage", "status": "false", "reason": "mount_failed"}
        )

    wait(
        blocked,
        300,
        lambda: f"erin's instance did not block: {admin.phase(receipt['instanceId'])}",
        interval=5,
    )
    missing = subprocess.run(
        ["docker", "exec", identifier, "test", "-e", "/srv/hermes-data/erin"],
        capture_output=True,
        timeout=30,
    )
    assert missing.returncode != 0, "a directory was created in place of the volume"
    command(
        [
            "docker",
            "exec",
            identifier,
            "sh",
            "-c",
            "mkdir /srv/hermes-data/erin && chown 1000:1000 /srv/hermes-data/erin",
        ]
    )
    wait(
        lambda: admin.phase(receipt["instanceId"])["phase"] == "ready",
        READY_TIMEOUT,
        lambda: (
            f"erin's instance did not recover: {admin.phase(receipt['instanceId'])}"
        ),
        interval=5,
    )


PLUGIN_PRELUDE = """
import json, sys, time
from pathlib import Path
sys.path[:0] = ["/opt/hermes", "/opt/hermes/plugins"]
import managed_execution

class Context:
    def __init__(self, runtime_id):
        self.runtime_id = runtime_id

    def get_config(self, key, default=""):
        return self.runtime_id if key == "runtime_id" else default

def environment(runtime_id, task, timeout=120):
    provider = managed_execution.Provider(Context(runtime_id))
    return provider.create_environment(cwd="/workspace", timeout=timeout, task_id=task)

def refused(runtime_id, task):
    try:
        environment(runtime_id, task, timeout=60).execute("true")
    except Exception as error:
        return str(error)
    return None
"""

TERMINAL_SCRIPT = (
    PLUGIN_PRELUDE
    + """
result = {}
result["available"] = managed_execution.Provider(Context("")).is_available()
shell = environment("", "e2e-cron")
reply = shell.execute(
    "id -u; id -g; pwd; printf terminal-ok > /workspace/e2e-terminal")
result["output"], result["returncode"] = reply["output"], reply["returncode"]
shell.cleanup()
denied = refused("not-allowed", "e2e-denied")
result["denied"] = bool(denied) and (
    "rejected" in denied.lower() or "not" in denied.lower())
print(json.dumps(result))
"""
)


def hermes_python(identifier, namespace, script, timeout=400):
    """Runs `script` with the Hermes Python in the owner's running instance;
    returns the JSON object its last line prints."""
    output = kubectl(
        identifier,
        "exec",
        "-i",
        "--namespace",
        namespace,
        "deployment/hermes",
        "-c",
        "hermes",
        "--",
        "/opt/hermes/.venv/bin/python",
        "-",
        data=script.encode(),
        timeout=timeout,
    )
    return json.loads(output.strip().splitlines()[-1])


def hermes_python_started(identifier, namespace, script):
    """The same in the background; read its output lines as they come."""
    process = subprocess.Popen(
        [
            "docker",
            "exec",
            "-i",
            identifier,
            "kubectl",
            "exec",
            "-i",
            "--namespace",
            namespace,
            "deployment/hermes",
            "-c",
            "hermes",
            "--",
            "/opt/hermes/.venv/bin/python",
            "-u",
            "-",
        ],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
    )
    process.stdin.write(script)
    process.stdin.close()
    return process


def environments(admin, instance_id):
    return admin.call("GET", f"/api/v1/instances/{instance_id}/environments").json()[
        "items"
    ]


def a_hermes_without_browser_traffic_acquires_a_terminal_backend(
    identifier, admin, assignments
):
    """The plugin in a running Hermes, with no browser involved, acquires the
    assignment's default backend, runs as the data identity in the shared
    workspace, and is refused a backend outside the assignment."""
    namespace = admin.phase(assignments["bob"]["instanceId"])["namespace"]
    output = kubectl(
        identifier,
        "exec",
        "-i",
        "--namespace",
        namespace,
        "deployment/hermes",
        "-c",
        "hermes",
        "--",
        "/opt/hermes/.venv/bin/python",
        "-",
        data=TERMINAL_SCRIPT.encode(),
        timeout=400,
    )
    result = json.loads(output.strip().splitlines()[-1])
    assert result["available"], "the plugin found no managed configuration"
    assert result["returncode"] == 0, result
    lines = result["output"].split()
    assert lines[:3] == ["1000", "1000", "/workspace"], result
    assert result["denied"], "a backend outside the assignment was served"
    shared = instance_pod_exec(identifier, namespace, "cat /workspace/e2e-terminal")
    assert shared == "terminal-ok", "the terminal backend wrote somewhere else"


def two_owners_reach_their_own_native_instances_through_the_gateway(
    identifier, admin, ca, passwords, assignments
):
    for name in ("bob", "carol"):
        browser = Browser(ca, name, passwords[name])
        landed = browser.sign_in()
        assert landed.status_code == 200, f"{name}: {landed.status_code}"
        me = browser.get("/_platform/me").json()
        assert me["identity"] == {"issuer": ISSUER, "subject": SUBJECTS[name]}
        native = browser.native_sign_in()
        assert native["user_id"] == SUBJECTS[name], f"{name} reached another owner"
    # Signing in enrolls nobody: no assignment and no namespace appear.
    before = namespaces(identifier)
    assigned = admin.call("GET", "/api/v1/assignments?limit=50").json()["items"]
    mallory = Browser(ca, "mallory", passwords["mallory"])
    denied = mallory.sign_in()
    assert denied.status_code == 403, f"mallory: {denied.status_code}"
    assert "No Hermes for this account" in denied.text
    time.sleep(5)
    assert namespaces(identifier) == before, "a login created a namespace"
    assert admin.call("GET", "/api/v1/assignments?limit=50").json()["items"] == assigned
    # Each instance has its own credentials and keys.
    held = {
        name: secret_digests(
            identifier,
            admin.phase(receipt["instanceId"])["namespace"],
            "hermes-credentials",
        )
        for name, receipt in assignments.items()
    }
    shared = set(held["bob"].values()) & set(held["carol"].values())
    assert held["bob"] and not shared, "two instances share credential material"


REVOKED_SCRIPT = (
    PLUGIN_PRELUDE
    + """
import signal, urllib.error, uuid
# Suspension stops Hermes soon after; poll fast and report every answer at
# once, so the refusal is seen before this process ends with the Pod.
signal.signal(signal.SIGTERM, signal.SIG_IGN)
settings = managed_execution.Provider(Context("")).settings()
client = managed_execution.Client(settings)
payload = {"session": "e2e-revoked", "mode": settings["mode"],
           "request": uuid.uuid4().hex}
last = None
deadline = time.monotonic() + 120
while time.monotonic() < deadline:
    try:
        client.call("/v1/leases/acquire", payload, timeout=10)
        print("granted", flush=True)
        break
    except urllib.error.HTTPError as error:
        if error.code != last:
            print(error.code, flush=True)
            last = error.code
        if error.code not in (409, 503):
            break
    time.sleep(0.1)
"""
)


def suspension_closes_open_connections_and_reenabling_keeps_data(
    identifier, admin, ca, passwords, assignments
):
    bob = Browser(ca, "bob", passwords["bob"])
    assert bob.sign_in().status_code == 200
    bob.native_sign_in()
    carol = Browser(ca, "carol", passwords["carol"])
    assert carol.sign_in().status_code == 200
    carol.native_sign_in()
    instance = assignments["bob"]["instanceId"]
    namespace = admin.phase(instance)["namespace"]
    marker = secrets.token_hex(8)
    instance_pod_exec(identifier, namespace, f"echo {marker} > /opt/data/e2e-marker")
    claims = kubectl(
        identifier,
        "get",
        "pvc",
        "--namespace",
        namespace,
        "-o",
        "jsonpath={.items[*].metadata.uid}",
    )

    # Suspended during an open chat connection and during provisioning.
    connection = bob.websocket()
    known = {e["id"] for e in environments(admin, instance)}
    provisioning = hermes_python_started(identifier, namespace, REVOKED_SCRIPT)
    pending = wait(
        lambda: [
            e
            for e in environments(admin, instance)
            if e["id"] not in known and e["session"] == "e2e-revoked"
        ],
        120,
        "the acquisition created no environment",
        interval=0.3,
    )
    admin.set_access(assignments["bob"]["assignmentId"], "suspended")
    took = closed_within(connection, 15)
    assert took < 12, f"open WebSocket outlived suspension by {took:.1f}s"
    output, _ = provisioning.communicate(timeout=180)
    answers = output.split()
    assert "granted" not in answers, "a lease was granted"
    assert answers[:1] == ["409"] and answers[-1] == "401", answers
    time.sleep(6)
    assert bob.get("/api/auth/me").status_code == 403
    assert carol.get("/api/auth/me").status_code == 200
    wait(
        lambda: admin.phase(instance)["phase"] == "retained",
        300,
        "bob's instance was not retained",
        interval=5,
    )
    drained = {e["id"]: e for e in environments(admin, instance)}[pending[0]["id"]]
    assert (drained["state"], drained["leases"]) == ("retired", 0), drained
    workers = kubectl(
        identifier, "get", "pods", "--namespace", namespace, "-o", "name"
    ).split()
    assert not workers, f"Pods left in a retained instance: {workers}"

    admin.set_access(assignments["bob"]["assignmentId"], "enabled")
    statuses = set()

    def back():
        reply = bob.get("/api/auth/me")
        statuses.add(reply.status_code)
        return reply.status_code == 200

    wait(back, READY_TIMEOUT, "bob's instance did not come back", interval=5)
    assert 202 in statuses, f"no owner status page while starting: {statuses}"
    assert admin.phase(instance)["id"] == instance
    kept = instance_pod_exec(identifier, namespace, "cat /opt/data/e2e-marker")
    assert kept.strip() == marker, "the home volume was replaced"
    assert (
        kubectl(
            identifier,
            "get",
            "pvc",
            "--namespace",
            namespace,
            "-o",
            "jsonpath={.items[*].metadata.uid}",
        )
        == claims
    )
    signed_out = bob.post("/auth/logout", allow_redirects=False)
    assert signed_out.status_code == 303
    assert signed_out.headers["Location"] == "/_platform/signed-out"
    again = bob.get("/", headers={"Accept": "text/html"}, allow_redirects=False)
    assert again.status_code == 303
    assert again.headers["Location"].startswith("/_platform/login")


def owners_share_a_static_folder(identifier, admin, assignments, template, terminal):
    """Two real instances and a worker share Carol's existing local folder."""
    receipt = admin.call(
        "POST",
        "/api/v1/assignments",
        {
            "identity": {"issuer": ISSUER, "subject": str(uuid.uuid4())},
            "spec": {
                "access": "enabled",
                "template": {"id": "personal", "version": template},
                "volumes": {
                    "workspace": {"source": "host", "path": "carol", "readOnly": True}
                },
                "terminal": terminal,
            },
        },
    ).json()
    wait(
        lambda: admin.phase(receipt["instanceId"])["phase"] == "ready",
        READY_TIMEOUT,
        "shared read-only instance did not become ready",
        interval=5,
    )
    events = admin.call(
        "GET",
        f"/api/v1/audit-events?targetKind=assignment&targetId={receipt['assignmentId']}",
    ).json()["items"]
    assert events[0]["change"]["volumes"]["workspace"] == {
        "source": "host",
        "path": "carol",
        "readOnly": True,
    }
    uses = admin.call("GET", "/api/v1/volumes?source=host").json()["items"]
    shared_uses = [use for use in uses if use.get("path") == "carol"]
    assert {(use["assignmentId"], use["readOnly"]) for use in shared_uses} == {
        (assignments["carol"]["assignmentId"], False),
        (receipt["assignmentId"], True),
    }
    writable = admin.phase(assignments["carol"]["instanceId"])["namespace"]
    readonly = admin.phase(receipt["instanceId"])["namespace"]
    marker = secrets.token_hex(8)
    instance_pod_exec(identifier, writable, f"printf {marker} > /workspace/e2e-drive")
    assert instance_pod_exec(identifier, readonly, "cat /workspace/e2e-drive") == marker
    # Invert the shell result so kubectl succeeds only when the write fails.
    denied = instance_pod_exec(
        identifier,
        readonly,
        "if touch /workspace/e2e-forbidden 2>/tmp/e2e-write-error; "
        "then exit 1; fi; cat /tmp/e2e-write-error",
    )
    assert "Read-only file system" in denied, denied
    script = (
        PLUGIN_PRELUDE
        + """
worker = environment("", "e2e-shared-drive")
result = worker.execute("cat /workspace/e2e-drive; "
                        "printf worker > /workspace/e2e-worker-drive")
worker.cleanup()
print(json.dumps(result))
"""
    )
    result = hermes_python(identifier, writable, script)
    assert result["returncode"] == 0 and result["output"].strip() == marker, result
    assert (
        instance_pod_exec(identifier, readonly, "cat /workspace/e2e-worker-drive")
        == "worker"
    )
    readonly_script = (
        PLUGIN_PRELUDE
        + """
worker = environment("", "e2e-readonly-drive")
result = worker.execute(
    "cat /workspace/e2e-worker-drive; "
    "if touch /workspace/e2e-worker-forbidden 2>/tmp/write-error; "
    "then exit 1; fi; grep -q 'Read-only file system' /tmp/write-error")
worker.cleanup()
print(json.dumps(result))
"""
    )
    result = hermes_python(identifier, readonly, readonly_script)
    assert result["returncode"] == 0 and result["output"].strip() == "worker", result
    pvs = json.loads(kubectl(identifier, "get", "pv", "-o", "json"))["items"]
    shared = [
        pv
        for pv in pvs
        if pv["spec"].get("local", {}).get("path") == "/srv/hermes-data/carol"
    ]
    assert len(shared) == 2 and len({pv["metadata"]["name"] for pv in shared}) == 2
    assert {pv["spec"]["claimRef"]["namespace"] for pv in shared} == {
        writable,
        readonly,
    }
    assert all(pv["spec"]["accessModes"] == ["ReadWriteOnce"] for pv in shared)


def namespaces(identifier):
    return set(
        kubectl(
            identifier, "get", "namespaces", "-o", "jsonpath={.items[*].metadata.name}"
        ).split()
    )


def secret_digests(identifier, namespace, name):
    """SHA-256 of each value of a Secret; the values are never printed."""
    data = json.loads(
        kubectl(
            identifier,
            "get",
            "secret",
            name,
            "--namespace",
            namespace,
            "-o",
            "json",
            quiet=True,
        )
    ).get("data", {})
    return {
        key: hashlib.sha256(base64.b64decode(value)).hexdigest()
        for key, value in data.items()
    }


def signed_in(ca, name, passwords):
    """A browser with platform and native sessions."""
    browser = Browser(ca, name, passwords[name])
    landed = browser.sign_in()
    assert landed.status_code == 200, f"{name}: {landed.status_code}"
    browser.native_sign_in()
    return browser


def closing(connection):
    """Watches `connection` in the background; the list receives the time the
    server closed it."""
    closed = []

    def watch():
        closed_within(connection, 900)
        closed.append(time.monotonic())

    threading.Thread(target=watch, daemon=True).start()
    return closed


def chat_settings_and_the_admin_secret_reach_only_the_referencing_instance(
    identifier, admin, ca, passwords, assignments
):
    """Bob's settings layer under the template's and assignment's settings and
    the platform's; the model key reaches only his instance, which the fake
    model sees, and no API returns it."""
    bob = signed_in(ca, "bob", passwords)
    saved = bob.put(
        "/api/config",
        json={
            "config": {
                # The web form's string form of `model.default`.
                "model": "user-model",
                "display": {"compact": False, "timestamps": True},
                "terminal": {"backend": "local"},
            }
        },
    )
    assert saved.status_code == 200, f"saving settings: {saved.status_code}"
    effective = bob.get("/api/config").json()
    assert effective["model"] == "fake-model", "the template's model lost"
    assert effective["display"]["compact"] is True, "the assignment lost"
    assert effective["terminal"]["backend"] == "managed_execution"
    assert effective["display"]["timestamps"] is True, "the owner's setting is gone"
    assert passwords["llm"] not in json.dumps(effective)
    bob_namespace = admin.phase(assignments["bob"]["instanceId"])["namespace"]
    # The owner's file keeps their own setting; managed values are never
    # written into it, and Hermes drops writes to managed keys.
    own = instance_pod_exec(identifier, bob_namespace, "cat /opt/data/config.yaml")
    assert yaml.safe_load(own)["display"] == {"timestamps": True}, "setting lost"
    for managed in ("fake-model", MODEL_URL, "api_key", "compact"):
        assert managed not in own, "a managed value was written to the owner's file"

    key = hashlib.sha256(passwords["llm"].encode()).hexdigest()
    seen = len(model_requests(identifier))
    session, text = bob.chat("hello")
    assert text == "hello from e2e", f"unexpected reply: {text[:200]}"
    requests_made = model_requests(identifier)[seen:]
    assert all(r["key"] == key for r in requests_made), "the model saw another key"
    models = {
        r["model"] for r in requests_made if r["path"].endswith("/chat/completions")
    }
    assert models == {"fake-model"}, f"the model was asked for {models}"
    history = bob.get("/api/sessions")
    assert history.status_code == 200, f"history: {history.status_code}"
    stored = session.get("stored_session_id") or session["session_id"]
    assert stored in history.text, "the conversation is not in Bob's history"

    carol = signed_in(ca, "carol", passwords)
    theirs = carol.get("/api/sessions")
    assert theirs.status_code == 200 and stored not in theirs.text
    carol_namespace = admin.phase(assignments["carol"]["instanceId"])["namespace"]
    delivered = instance_pod_exec(
        identifier, bob_namespace, 'printf %s "$OPENAI_API_KEY" | sha256sum'
    )
    assert delivered.split()[0] == key, "Bob's instance lacks the model key"
    missing = subprocess.run(
        [
            "docker",
            "exec",
            identifier,
            "kubectl",
            "exec",
            "--namespace",
            carol_namespace,
            "deployment/hermes",
            "-c",
            "hermes",
            "--",
            "printenv",
            "OPENAI_API_KEY",
        ],
        capture_output=True,
        timeout=60,
    )
    assert missing.returncode != 0, "Carol's instance received the model key"
    encoded = base64.b64encode(passwords["llm"].encode()).decode()
    for namespace, present in ((bob_namespace, True), (carol_namespace, False)):
        listing = kubectl(
            identifier,
            "get",
            "secrets",
            "--namespace",
            namespace,
            "-o",
            "json",
            quiet=True,
        )
        assert (encoded in listing) is present, (
            f"model key in {namespace}: {not present}"
        )
    for path in ("/api/v1/secrets", "/api/v1/audit-events?limit=50"):
        assert passwords["llm"] not in admin.call("GET", path).text, path
        assert encoded not in admin.call("GET", path).text, path


def uploads_reach_only_the_owners_private_home(
    identifier, admin, ca, passwords, assignments
):
    """Native file management, rooted in the private Hermes home: Bob's upload
    lands in his home only, and the gateway bounds its size."""
    bob = signed_in(ca, "bob", passwords)
    carol = signed_in(ca, "carol", passwords)
    marker = secrets.token_hex(8)
    uploaded = bob.upload("uploads/e2e.txt", marker.encode())
    assert uploaded.status_code == 200, f"upload: {uploaded.status_code}"
    listing = bob.get("/api/files", params={"path": "uploads"})
    assert listing.status_code == 200 and "e2e.txt" in listing.text
    theirs = carol.get("/api/files", params={"path": "uploads"})
    assert "e2e.txt" not in theirs.text, "Carol sees Bob's upload"
    bob_namespace = admin.phase(assignments["bob"]["instanceId"])["namespace"]
    carol_namespace = admin.phase(assignments["carol"]["instanceId"])["namespace"]
    stored = instance_pod_exec(
        identifier, bob_namespace, "cat /opt/data/uploads/e2e.txt"
    )
    assert stored == marker, "the upload did not reach Bob's home"
    elsewhere = instance_pod_exec(
        identifier, carol_namespace, "test -e /opt/data/uploads/e2e.txt; echo $?"
    )
    assert elsewhere.strip() == "1", "the upload reached Carol's instance"
    # The gateway bounds bodies at 64 MiB before an instance sees them.
    oversized = bob.post(
        "/api/files/upload",
        data=b"x" * (64 * 1024 * 1024 + 1),
        headers={"Content-Type": "application/json"},
        timeout=120,
    )
    assert oversized.status_code == 413, f"oversized upload: {oversized.status_code}"


def a_cron_job_fires_without_browser_traffic_and_acquires_a_terminal(
    identifier, admin, ca, passwords, assignments
):
    """A recurring job saved by Bob fires on its own, with no browser
    connected, and its terminal command runs in his backend."""
    instance = assignments["bob"]["instanceId"]
    namespace = admin.phase(instance)["namespace"]
    known = {e["id"] for e in environments(admin, instance)}
    marker = secrets.token_hex(8)
    bob = signed_in(ca, "bob", passwords)
    job = bob.post(
        "/api/cron/jobs",
        json={
            "name": "e2e",
            "schedule": "every 1m",
            "prompt": f"run: printf cron-{marker} > /workspace/e2e-cron",
            "deliver": "local",
        },
    )
    assert job.status_code == 200, f"cron job: {job.status_code} {job.text[:200]}"
    job_id = job.json()["id"]
    bob.session.close()
    fired = wait(
        lambda: (
            instance_pod_exec(
                identifier, namespace, "cat /workspace/e2e-cron 2>/dev/null || true"
            )
            == f"cron-{marker}"
        ),
        420,
        "the cron job never ran its command",
        interval=10,
    )
    assert fired
    acquired = [e for e in environments(admin, instance) if e["id"] not in known]
    assert acquired, "the cron run acquired no terminal environment"
    ready = kubectl(
        identifier,
        "get",
        "deployment",
        "hermes",
        "--namespace",
        namespace,
        "-o",
        "jsonpath={.spec.replicas} {.status.readyReplicas}",
    )
    assert ready.split() == ["1", "1"], f"Hermes replicas: {ready}"
    bob = signed_in(ca, "bob", passwords)
    removed = bob.session.delete(
        f"https://{PUBLIC}/api/cron/jobs/{job_id}",
        headers={"Origin": f"https://{PUBLIC}"},
        timeout=30,
    )
    assert removed.status_code == 200, f"removing the job: {removed.status_code}"


TOOLS_SCRIPT = (
    PLUGIN_PRELUDE
    + """
result = {}
# The private Hermes home, where uploads land, is never in a worker.
tools = environment("tools", "e2e-tools")
reply = tools.execute(
    "id -u; ls -d /home/worker/home && printf tools-ok > /home/worker/home/e2e-tools;"
    " cat /workspace/e2e-shared; echo; test -e /opt/data/uploads/e2e.txt; echo $?")
result["tools"] = [reply["output"], reply["returncode"]]
result["tools_environment"] = tools.managed_environment
# Released but not closed: the environment idles until the backend retires it.
tools._release_lease()
shell = environment("", "e2e-shell")
reply = shell.execute(
    "test -e /home/worker/home; echo $?; cat /workspace/e2e-shared; echo;"
    " test -e /opt/data/uploads/e2e.txt; echo $?")
result["shell"] = [reply["output"], reply["returncode"]]
shell.cleanup()
print(json.dumps(result))
"""
)


def hermes_pod(identifier, namespace):
    """The UID of the instance's Hermes Pod (worker Pods are `hw-…`)."""
    pods = kubectl(
        identifier,
        "get",
        "pods",
        "--namespace",
        namespace,
        "-o",
        'jsonpath={range .items[*]}{.metadata.name} {.metadata.uid}{"\\n"}{end}',
    )
    uids = [
        line.split()[1]
        for line in pods.splitlines()
        if line.strip() and not line.startswith("hw-")
    ]
    assert len(uids) == 1, f"Hermes Pods: {pods}"
    return uids[0]


def bobs_backends_share_the_workspace_and_idle_workers_retire(
    identifier, admin, assignments
):
    """Bob's example: the workspace holds the same files in Hermes and both
    backends; his network home is mounted only in `tools`, his private Hermes
    home in neither. An idle `tools` worker retires without touching Hermes."""
    instance = assignments["bob"]["instanceId"]
    namespace = admin.phase(instance)["namespace"]
    pod = hermes_pod(identifier, namespace)
    shared = secrets.token_hex(8)
    instance_pod_exec(identifier, namespace, f"printf {shared} > /workspace/e2e-shared")
    result = hermes_python(identifier, namespace, TOOLS_SCRIPT)
    output, code = result["tools"]
    expected = ["1000", "/home/worker/home", shared, "1"]
    assert code == 0 and output.split() == expected, output
    output, code = result["shell"]
    assert code == 0 and output.split() == ["1", shared, "1"], output
    written = command(
        ["docker", "exec", identifier, "cat", "/srv/hermes-data/homes/bob/e2e-tools"]
    )
    assert written == "tools-ok", "the network home is not Bob's host path"
    claims = kubectl(
        identifier,
        "get",
        "deployment",
        "hermes",
        "--namespace",
        namespace,
        "-o",
        "jsonpath={.spec.template.spec.volumes[*].persistentVolumeClaim.claimName}",
    ).split()
    assert "vol-home" not in claims, f"Hermes mounts the backend-only home: {claims}"

    environment = result["tools_environment"]

    def retired():
        states = {e["id"]: e["state"] for e in environments(admin, instance)}
        return states.get(environment) == "retired"

    wait(retired, 300, "the idle tools worker never retired", interval=5)
    assert hermes_pod(identifier, namespace) == pod, "an idle worker restarted Hermes"
    kept = instance_pod_exec(identifier, namespace, "cat /workspace/e2e-shared")
    assert kept == shared, "the workspace changed"


def a_backend_identity_controls_its_workers_and_keeps_the_hermes_home_private(
    identifier, admin, template, worker
):
    """A backend-only folder belongs to its worker identity; mismatches fail."""
    with tempfile.TemporaryDirectory(prefix="hermes-other-identity-") as temporary:
        image = import_worker_image(identifier, temporary, uid=2000, gid=3000)
    folder = "other-user-" + uuid.uuid4().hex[:8]
    command(
        [
            "docker",
            "exec",
            identifier,
            "sh",
            "-c",
            f"mkdir /srv/hermes-data/{folder} && "
            f"chown 2000:3000 /srv/hermes-data/{folder} && "
            f"chmod 700 /srv/hermes-data/{folder}",
        ]
    )
    definition = {
        **worker,
        "uid": 2000,
        "gid": 3000,
        "limits": {"idleSeconds": 60, "maxLeaseSeconds": 900, "maxEnvironments": 2},
        "mounts": {"drive": {"mountPath": "/drive"}},
    }
    good = admin.publish("terminal-backends", folder, {**definition, "image": image})
    bad_id = folder + "-bad"
    bad = admin.publish("terminal-backends", bad_id, definition)
    receipt = admin.call(
        "POST",
        "/api/v1/assignments",
        {
            "identity": {"issuer": ISSUER, "subject": str(uuid.uuid4())},
            "spec": {
                "access": "enabled",
                "template": {"id": "personal", "version": template},
                "volumes": {"drive": {"source": "host", "path": folder}},
                "terminal": {"default": folder, "allowed": {folder: good, bad_id: bad}},
            },
        },
    ).json()
    instance = receipt["instanceId"]
    wait(
        lambda: admin.phase(instance)["phase"] == "ready",
        READY_TIMEOUT,
        "the alternate-identity assignment did not become ready",
        interval=5,
    )
    namespace = admin.phase(instance)["namespace"]
    script = (
        PLUGIN_PRELUDE
        + """
provider = managed_execution.Provider(Context("BACKEND"))
worker = provider.create_environment(
    cwd="/drive", timeout=120, task_id="other-identity")
result = worker.execute("id -u; id -g; printf owned > /drive/created; "
                        "test ! -e /opt/data")
worker.cleanup()
print(json.dumps(result))
"""
    )
    result = hermes_python(identifier, namespace, script.replace("BACKEND", folder))
    assert result["returncode"] == 0 and result["output"].split() == ["2000", "3000"]
    ownership = command(
        [
            "docker",
            "exec",
            identifier,
            "stat",
            "-c",
            "%u:%g",
            f"/srv/hermes-data/{folder}/created",
        ]
    )
    assert ownership.strip() == "2000:3000", ownership
    home = instance_pod_exec(
        identifier,
        namespace,
        "id -u; id -g; stat -c '%u:%g' /opt/data; test ! -e /drive",
    )
    assert home.split() == ["1000", "1000", "1000:1000"], home
    failure_script = (
        PLUGIN_PRELUDE
        + """
try:
    provider = managed_execution.Provider(Context("BACKEND"))
    worker = provider.create_environment(
        cwd="/drive", timeout=120, task_id="bad-identity")
    worker.execute("true")
except Exception:
    print(json.dumps({"refused": True}))
else:
    worker.cleanup()
    print(json.dumps({"refused": False}))
"""
    )
    result = hermes_python(
        identifier, namespace, failure_script.replace("BACKEND", bad_id)
    )
    assert result["refused"], "an image without the declared worker identity ran"
    failures = [
        e["failure"]
        for e in environments(admin, instance)
        if e["backend"]["id"] == bad_id
    ]
    assert failures == ["contract_failed"], failures


CHANGE_SCRIPT = (
    PLUGIN_PRELUDE
    + """
held = environment("", "e2e-held")
codes = [held.execute("echo first")["returncode"]]
print(json.dumps({"held": held.managed_environment}), flush=True)
go = Path("/tmp/e2e-change-go")
deadline = time.monotonic() + 300
while not go.exists() and time.monotonic() < deadline:
    time.sleep(1)
codes.append(held.execute("echo second")["returncode"])
fresh = environment("", "e2e-fresh")
codes.append(fresh.execute("echo third")["returncode"])
result = {"held": held.managed_environment, "fresh": fresh.managed_environment,
          "codes": codes}
held.cleanup()
fresh.cleanup()
print(json.dumps(result))
"""
)


def change_terminal(admin, assignment_id, change):
    """Applies `change(terminal)` to an assignment's terminal grant."""
    current, etag = admin.assignment(assignment_id)
    terminal = json.loads(json.dumps(current["spec"]["terminal"]))
    change(terminal)
    return admin.call(
        "PATCH",
        f"/api/v1/assignments/{assignment_id}",
        {"terminal": terminal},
        etag=etag,
    ).json()


def a_backend_change_keeps_active_leases_and_drains_superseded_environments(
    identifier, admin, assignments, worker, versions
):
    """A new backend version during an active lease: the lease keeps its
    environment, new acquisitions use the new version, and the superseded
    environment drains once its lease ends."""
    instance = assignments["bob"]["instanceId"]
    namespace = admin.phase(instance)["namespace"]
    pod = hermes_pod(identifier, namespace)
    process = hermes_python_started(identifier, namespace, CHANGE_SCRIPT)
    try:
        held = json.loads(process.stdout.readline())["held"]
        generation = admin.phase(instance)["executionGeneration"]
        version = admin.publish(
            "terminal-backends",
            "shell",
            {
                **worker,
                "resources": {**worker["resources"], "memoryMib": 384},
                "limits": {
                    "idleSeconds": 300,
                    "maxLeaseSeconds": 900,
                    "maxEnvironments": 4,
                },
                "mounts": {"workspace": {"mountPath": "/workspace"}},
            },
        )
        change_terminal(
            admin,
            assignments["bob"]["assignmentId"],
            lambda terminal: terminal["allowed"].update(shell=version),
        )
        wait(
            lambda: admin.phase(instance)["executionGeneration"] > generation,
            60,
            "the backend change did not advance the execution generation",
            interval=1,
        )
        instance_pod_exec(identifier, namespace, "touch /tmp/e2e-change-go")
        output, _ = process.communicate(timeout=400)
    finally:
        if process.poll() is None:
            process.kill()
    result = json.loads(output.strip().splitlines()[-1])
    assert result["codes"] == [0, 0, 0], result
    assert result["held"] == held != result["fresh"], result
    listed = {e["id"]: e for e in environments(admin, instance)}
    assert listed[held]["backend"] == {"id": "shell", "version": versions["shell"]}
    assert listed[result["fresh"]]["backend"] == {"id": "shell", "version": version}
    wait(
        lambda: (
            {e["id"]: e["state"] for e in environments(admin, instance)}[held]
            == "retired"
        ),
        360,
        "the superseded environment never drained",
        interval=5,
    )
    assert hermes_pod(identifier, namespace) == pod, "a backend change restarted Hermes"
    versions["shell"] = version


PRIVATE_SCRIPT = (
    PLUGIN_PRELUDE
    + """
result = {}
try:
    private = environment("private", "e2e-private-TASK")
    reply = private.execute("id -u")
    result["output"], result["returncode"] = reply["output"], reply["returncode"]
    private.cleanup()
except Exception as error:
    result["error"] = str(error)
print(json.dumps(result))
"""
)


def push_image(tag, base, repository, credentials):
    """Uploads a local image's linux/amd64 manifest and blobs through the
    registry API (Docker refuses plain HTTP pushes); returns its digest."""
    archive = subprocess.run(
        ["docker", "save", tag], capture_output=True, timeout=600, check=True
    ).stdout
    bundle = tarfile.open(fileobj=io.BytesIO(archive))

    def blob(digest):
        return bundle.extractfile("blobs/sha256/" + digest.split(":", 1)[1]).read()

    session = requests.Session()
    session.auth = credentials

    def manifest(descriptor):
        document = json.loads(blob(descriptor["digest"]))
        for child in document.get("manifests", []):
            platform = child.get("platform", {})
            if (platform.get("os"), platform.get("architecture")) == ("linux", "amd64"):
                return manifest(child)
        assert "layers" in document, "no linux/amd64 image manifest"
        return descriptor, document

    index = json.loads(bundle.extractfile("index.json").read())
    descriptor, document = manifest(index["manifests"][0])
    for layer in [document["config"], *document["layers"]]:
        digest = layer["digest"]
        if session.head(f"{base}/v2/{repository}/blobs/{digest}", timeout=30).ok:
            continue
        started = session.post(f"{base}/v2/{repository}/blobs/uploads/", timeout=30)
        assert started.status_code == 202, f"blob upload: {started.status_code}"
        location = started.headers["Location"]
        if not location.startswith("http"):
            location = base + location
        stored = session.put(
            location,
            params={"digest": digest},
            data=blob(digest),
            headers={"Content-Type": "application/octet-stream"},
            timeout=300,
        )
        assert stored.status_code == 201, f"blob: {stored.status_code}"
    stored = session.put(
        f"{base}/v2/{repository}/manifests/{descriptor['digest']}",
        data=blob(descriptor["digest"]),
        headers={"Content-Type": document.get("mediaType") or descriptor["mediaType"]},
        timeout=60,
    )
    assert stored.status_code == 201, f"manifest: {stored.status_code}"
    return descriptor["digest"]


@contextmanager
def private_registry(identifier, network, temporary, password, run):
    """A registry behind basic authentication on the fixture network holding
    the worker image; yields the image's digest reference there. The node
    resolves the registry's name through its hosts file: the default bridge
    network has no container DNS."""
    suffix = uuid.uuid4().hex[:8]
    upstream = f"personal-hermes-registry-{suffix}"
    proxy = f"personal-hermes-registry-proxy-{suffix}"
    labels = [
        "--label",
        f"{FIXTURE}={REGISTRY_FIXTURE}",
        "--label",
        f"{RUN}={run}",
    ]
    command(["docker", "pull", "--quiet", REGISTRY_IMAGE], timeout=600)
    with Cleanup() as cleanup:
        command(
            [
                "docker",
                "run",
                "--detach",
                "--name",
                upstream,
                *labels,
                "--network",
                network,
                REGISTRY_IMAGE,
            ]
        )
        cleanup.add(upstream, lambda: remove_fixture(upstream, REGISTRY_FIXTURE, run))
        settings = Path(temporary) / "registry.env"
        settings.write_text(
            f"UPSTREAM={container_ip(upstream, network)}:5000\n"
            f"CREDENTIALS=e2e:{password}\n"
        )
        settings.chmod(0o600)
        command(
            [
                "docker",
                "run",
                "--detach",
                "--name",
                proxy,
                *labels,
                "--network",
                network,
                "--env-file",
                str(settings),
                "--volume",
                f"{HERE / 'registry_proxy.py'}:/proxy/registry_proxy.py:ro",
                "--entrypoint",
                "/opt/hermes/.venv/bin/python",
                "--publish",
                "127.0.0.1::5000",
                HERMES_IMAGE,
                "/proxy/registry_proxy.py",
            ]
        )
        cleanup.add(proxy, lambda: remove_fixture(proxy, REGISTRY_FIXTURE, run))
        host = REGISTRY.split(":")[0]
        command(
            [
                "docker",
                "exec",
                identifier,
                "sh",
                "-c",
                # Replaces an earlier run's entry on a reused cluster.
                f"kept=$(grep -v ' {host}$' /etc/hosts); "
                f"printf '%s\\n%s\\n' \"$kept\" '{container_ip(proxy, network)} {host}'"
                " > /etc/hosts",
            ]
        )
        state = json.loads(command(["docker", "inspect", proxy]))[0]
        port = state["NetworkSettings"]["Ports"]["5000/tcp"][0]["HostPort"]
        base = f"http://127.0.0.1:{port}"
        wait(
            lambda: requests.get(f"{base}/v2/", timeout=3).status_code == 401,
            60,
            "the private registry did not start",
            interval=1,
        )
        digest = push_image(WORKER_TAG, base, "hermes-worker", ("e2e", password))
        yield f"{REGISTRY}/hermes-worker@{digest}"


def a_private_backend_pulls_with_its_registry_secret_and_reports_failures(
    identifier, admin, assignments, images, passwords, worker
):
    """A backend from a private registry: wrong credentials are reported as
    `image_unavailable` and never replaced by another image; after the
    registry secret is corrected the worker pulls and runs."""
    instance = assignments["bob"]["instanceId"]
    namespace = admin.phase(instance)["namespace"]

    def registry_secret(password):
        auth = base64.b64encode(f"e2e:{password}".encode()).decode()
        admin.call(
            "PUT",
            "/api/v1/secrets/e2e-registry",
            {
                "kind": "registry",
                "value": json.dumps({"auths": {REGISTRY: {"auth": auth}}}),
            },
        )

    def converged():
        view = admin.phase(instance)
        return (
            view["phase"] == "ready"
            and view["observedGeneration"] == view["desiredGeneration"]
        )

    registry_secret("wrong-" + passwords["registry"])
    private = admin.publish(
        "terminal-backends",
        "private",
        {
            **worker,
            "image": images["private"],
            "registrySecret": "e2e-registry",
            "limits": {"idleSeconds": 60, "maxLeaseSeconds": 900, "maxEnvironments": 2},
            "mounts": {"workspace": {"mountPath": "/workspace"}},
        },
    )
    change_terminal(
        admin,
        assignments["bob"]["assignmentId"],
        lambda terminal: terminal["allowed"].update(private=private),
    )
    wait(
        converged, READY_TIMEOUT, "Bob's instance did not take the backend", interval=5
    )
    known = {e["id"] for e in environments(admin, instance)}
    result = hermes_python(identifier, namespace, PRIVATE_SCRIPT.replace("TASK", "1"))
    assert "(422)" in result.get("error", ""), f"wrong credentials: {result}"
    failed = [
        e["failure"]
        for e in environments(admin, instance)
        if e["id"] not in known and e["backend"]["id"] == "private"
    ]
    assert failed == ["image_unavailable"], failed

    registry_secret(passwords["registry"])
    wait(converged, READY_TIMEOUT, "Bob's instance did not take the secret", interval=5)
    result = hermes_python(identifier, namespace, PRIVATE_SCRIPT.replace("TASK", "2"))
    assert result.get("returncode") == 0, f"right credentials: {result}"
    assert result["output"].split() == ["1000"], result
    pulled = command(["docker", "exec", identifier, "crictl", "images", "-o", "json"])
    assert REGISTRY + "/hermes-worker" in pulled, "the worker image was not pulled"


def cookie_header(browser, native):
    """`browser`'s platform cookies, or its native ones."""
    return "; ".join(
        f"{cookie.name}={cookie.value}"
        for cookie in browser.session.cookies
        if cookie.domain.lstrip(".") == PUBLIC
        and ("hermes-platform" in cookie.name.lower()) != native
    )


def foreign_cookies_tickets_and_forged_headers_reach_nothing(ca, passwords):
    """Carol's native cookie or WebSocket ticket in Bob's browser reaches only
    Bob's instance, which refuses it; forged identity headers change nothing."""
    bob = signed_in(ca, "bob", passwords)
    carol = signed_in(ca, "carol", passwords)
    raw = requests.Session()
    raw.verify = ca
    foreign = raw.get(
        f"https://{PUBLIC}/api/auth/me",
        headers={
            "Cookie": cookie_header(bob, False) + "; " + cookie_header(carol, True)
        },
        timeout=30,
    )
    assert foreign.status_code in (401, 403), (
        f"foreign native cookie: {foreign.status_code}"
    )
    alone = raw.get(
        f"https://{PUBLIC}/api/auth/me",
        headers={"Cookie": cookie_header(carol, True), "Accept": "text/html"},
        allow_redirects=False,
        timeout=30,
    )
    assert alone.status_code in (303, 401), f"no platform session: {alone.status_code}"

    ticket = carol.post("/api/auth/ws-ticket").json()["ticket"]
    try:
        connection = websocket.create_connection(
            f"wss://{PUBLIC}/api/events?channel=e2e&ticket={ticket}",
            header=[f"Cookie: {bob.cookie_header()}"],
            origin=f"https://{PUBLIC}",
            sslopt={"ca_certs": ca, "cert_reqs": ssl.CERT_REQUIRED},
            timeout=20,
        )
    except websocket.WebSocketException:
        pass
    else:
        assert closed_within(connection, 10) < 10, "a foreign ticket was accepted"

    forged = bob.get(
        "/api/auth/me",
        headers={
            "X-Forwarded-User": SUBJECTS["carol"],
            "X-Forwarded-Email": "carol@e2e.test",
            "X-Hermes-Owner": SUBJECTS["carol"],
            "X-Auth-Request-User": SUBJECTS["carol"],
            "Remote-User": SUBJECTS["carol"],
        },
    )
    assert forged.status_code == 200 and forged.json()["user_id"] == SUBJECTS["bob"]


def an_email_change_keeps_ownership_and_the_old_email_gets_nothing(
    ca, passwords, assignments
):
    directory = Directory(ca, passwords["directory"])
    directory.update(SUBJECTS["bob"], email="robert@e2e.test")
    bob = signed_in(ca, "bob", passwords)
    me = bob.get("/_platform/me").json()
    assert me["identity"] == {"issuer": ISSUER, "subject": SUBJECTS["bob"]}
    assert bob.get("/api/auth/me").json()["user_id"] == SUBJECTS["bob"]
    password = secrets.token_urlsafe(18)
    subject = directory.create("bob-new", "bob@e2e.test", password)
    assert subject != SUBJECTS["bob"]
    newcomer = Browser(ca, "bob-new", password)
    denied = newcomer.sign_in()
    assert denied.status_code == 403, f"the old email reached: {denied.status_code}"
    assert "No Hermes for this account" in denied.text


def scale(identifier, namespace, deployment, replicas):
    kubectl(
        identifier,
        "scale",
        "--namespace",
        namespace,
        f"deployment/{deployment}",
        f"--replicas={replicas}",
    )
    if replicas:
        kubectl(
            identifier,
            "rollout",
            "status",
            "--namespace",
            namespace,
            f"deployment/{deployment}",
            "--timeout=300s",
            timeout=320,
        )
    else:
        wait(
            lambda: (
                not kubectl(
                    identifier,
                    "get",
                    "pods",
                    "--namespace",
                    namespace,
                    "--selector",
                    f"app.kubernetes.io/name={deployment}"
                    if namespace == NAMESPACE
                    else f"app={deployment}",
                    "-o",
                    "name",
                ).strip()
            ),
            180,
            f"{deployment} did not stop",
            interval=1,
        )


def denied_within(bob, connection_closed, started, requests_bound, close_bound):
    """New requests fail closed within `requests_bound` seconds of `started`
    and the open WebSocket closes within `close_bound`."""
    wait(
        lambda: bob.get("/api/auth/me", allow_redirects=False).status_code != 200,
        requests_bound + 10,
        "requests still reached the instance",
        interval=0.5,
    )
    took = time.monotonic() - started
    assert took <= requests_bound, f"requests were denied only after {took:.1f}s"
    wait(lambda: connection_closed, close_bound + 10, "the WebSocket stayed open", 0.5)
    took = connection_closed[0] - started
    assert took <= close_bound, f"the WebSocket closed only after {took:.1f}s"


def outages_and_provider_revocation_fail_closed_within_the_bounds(
    identifier, postgres, admin, ca, passwords, assignments
):
    """No manager, no database, a person disabled at the provider, no
    provider: each denies within the bounds and nothing falls back."""
    # The manager's API, which resolves routes.
    bob = signed_in(ca, "bob", passwords)
    closed = closing(bob.websocket())
    scale(identifier, NAMESPACE, "hermes-manager-api", 0)
    denied_within(bob, closed, time.monotonic(), 8, 12)
    scale(identifier, NAMESPACE, "hermes-manager-api", 1)
    wait(lambda: bob.get("/api/auth/me").status_code == 200, 120, "no recovery")

    # The database.
    closed = closing(bob.websocket())
    command(["docker", "pause", postgres])
    try:
        denied_within(bob, closed, time.monotonic(), 8, 12)
    finally:
        command(["docker", "unpause", postgres])
    wait(lambda: bob.get("/api/auth/me").status_code == 200, 120, "no recovery")

    # The provider disables Bob: his session ends within 130 s.
    directory = Directory(ca, passwords["directory"])
    closed = closing(bob.websocket())
    directory.update(SUBJECTS["bob"], enabled=False)
    denied_within(bob, closed, time.monotonic(), 130, 130)
    directory.update(SUBJECTS["bob"], enabled=True)

    # No provider: evidence older than 120 s is refused, and nobody signs in.
    bob = signed_in(ca, "bob", passwords)
    closed = closing(bob.websocket())
    scale(identifier, "keycloak", "keycloak", 0)
    denied_within(bob, closed, time.monotonic(), 130, 130)
    page = bob.get("/", headers={"Accept": "text/html"}, allow_redirects=False)
    assert page.status_code != 200, "a page was served without a provider"
    scale(identifier, "keycloak", "keycloak", 1)
    wait(
        lambda: (
            requests.get(
                f"{ISSUER}/.well-known/openid-configuration", verify=ca, timeout=5
            ).status_code
            == 200
        ),
        300,
        "Keycloak did not come back",
    )
    # The restarted provider imported the realm again; everyone signs in.
    assert signed_in(ca, "bob", passwords).get("/api/auth/me").status_code == 200
    assert admin.call("GET", "/api/v1/session").status_code == 200


def only_the_controller_holds_kubernetes_credentials_and_logs_hold_no_secrets(
    identifier, admin, ca, passwords, assignments
):
    controller = f"system:serviceaccount:{NAMESPACE}:hermes-manager-controller"
    bob_namespace = admin.phase(assignments["bob"]["instanceId"])["namespace"]

    def can(user, *request):
        return (
            kubectl(identifier, "auth", "can-i", *request, "--as", user, check=False)
            .strip()
            .startswith("yes")
        )

    assert can(controller, "create", "pods", "--namespace", bob_namespace)
    for request in (
        ("create", "pods", "--namespace", "kube-system"),
        ("get", "secrets", "--namespace", "kube-system"),
        ("get", "secrets", "--namespace", NAMESPACE),
        ("delete", "namespaces"),
        ("create", "clusterrolebindings"),
        ("delete", "persistentvolumes"),
    ):
        assert not can(controller, *request), f"the controller may {request}"
    for account in ("default",):
        user = f"system:serviceaccount:{NAMESPACE}:{account}"
        for request in (
            ("create", "pods", "--namespace", bob_namespace),
            ("list", "namespaces"),
            ("get", "secrets", "--namespace", NAMESPACE),
        ):
            assert not can(user, *request), f"{account} may {request}"
    mounted = subprocess.run(
        [
            "docker",
            "exec",
            identifier,
            "kubectl",
            "exec",
            "--namespace",
            bob_namespace,
            "deployment/hermes",
            "-c",
            "hermes",
            "--",
            "test",
            "-e",
            "/var/run/secrets/kubernetes.io/serviceaccount/token",
        ],
        capture_output=True,
        timeout=60,
    )
    assert mounted.returncode != 0, "Hermes holds a Kubernetes token"
    # The execution credential works only against the execution interface.
    token = instance_pod_exec(
        identifier,
        bob_namespace,
        "cat /run/hermes/credentials/execution-client-token",
    ).strip()
    for url in (f"https://{ADMIN}/api/v1/session", f"https://{PUBLIC}/_platform/me"):
        reply = requests.get(
            url, headers={"Authorization": f"Bearer {token}"}, verify=ca, timeout=20
        )
        assert reply.status_code == 401, f"{url}: {reply.status_code}"

    secret_values = [
        value
        for name, value in passwords.items()
        if name in ("llm", "registry", "directory", *SUBJECTS)
    ]
    sources = [
        (NAMESPACE, f"deployment/{name}")
        for name in (
            "hermes-manager-api",
            "hermes-manager-controller",
            "hermes-gateway",
        )
    ] + [
        (admin.phase(receipt["instanceId"])["namespace"], "deployment/hermes")
        for receipt in assignments.values()
    ]
    for namespace, target in sources:
        logs = kubectl(
            identifier,
            "logs",
            "--namespace",
            namespace,
            target,
            "--all-containers",
            "--tail=-1",
            quiet=True,
        )
        for value in secret_values:
            assert value not in logs, f"a fixture secret appears in {target} logs"


def an_instance_missing_from_a_restored_backup_is_reported_not_adopted(
    identifier, postgres, database, ca, passwords, assignments
):
    """PostgreSQL restored from a backup older than an instance, on the same
    cluster: the instance's namespace and volume are orphans, reported and
    left alone, and their owner gets nothing."""
    admin = Admin(ca, passwords["alice"])
    backup = subprocess.run(
        ["docker", "exec", postgres, "pg_dump", "-U", "hermes_test", "-Fc", database],
        capture_output=True,
        timeout=300,
        check=True,
    ).stdout
    spec, _ = admin.assignment(assignments["carol"]["assignmentId"])
    command(
        [
            "docker",
            "exec",
            identifier,
            "sh",
            "-c",
            "mkdir -p /srv/hermes-data/mallory"
            " && chown 1000:1000 /srv/hermes-data/mallory",
        ]
    )
    receipt = admin.call(
        "POST",
        "/api/v1/assignments",
        {
            "identity": {"issuer": ISSUER, "subject": SUBJECTS["mallory"]},
            "spec": {
                **spec["spec"],
                "volumes": {"workspace": {"source": "host", "path": "mallory"}},
            },
        },
    ).json()
    namespace = admin.phase(receipt["instanceId"])["namespace"]
    wait(
        lambda: admin.phase(receipt["instanceId"])["phase"] == "ready",
        READY_TIMEOUT,
        "the instance after the backup never became ready",
        interval=5,
    )

    for name in ("hermes-manager-api", "hermes-manager-controller"):
        scale(identifier, NAMESPACE, name, 0)
    psql = ["docker", "exec", "-i", postgres, "psql", "-U", "hermes_test", "-d"]
    command([*psql, "hermes_test", "-c", f"DROP DATABASE {database} WITH (FORCE)"])
    command([*psql, "hermes_test", "-c", f"CREATE DATABASE {database}"])
    subprocess.run(
        [
            "docker",
            "exec",
            "-i",
            postgres,
            "pg_restore",
            "-U",
            "hermes_test",
            "-d",
            database,
            "--no-owner",
        ],
        input=backup,
        capture_output=True,
        timeout=300,
        check=True,
    )
    for name in ("hermes-manager-api", "hermes-manager-controller"):
        scale(identifier, NAMESPACE, name, 1)

    volume = f"{namespace}-workspace"
    expected = {("namespace", namespace), ("persistent_volume", volume)}

    def reported():
        report = admin.call("GET", "/api/v1/orphans").json()
        return {(item["kind"], item["name"]) for item in report["items"]} == expected

    wait(reported, 180, "the orphaned namespace and volume were not reported")
    admin.call("GET", f"/api/v1/instances/{receipt['instanceId']}", expected=(404,))
    for kind, name in (("namespace", namespace), ("pv", volume)):
        phase = kubectl(identifier, "get", kind, name, "-o", "jsonpath={.status.phase}")
        assert phase in ("Active", "Bound"), f"the orphaned {kind} changed: {phase}"
    mallory = Browser(ca, "mallory", passwords["mallory"])
    assert mallory.sign_in().status_code == 403, "an orphan was routed"


def restore_on_a_new_cluster_rebinds_without_adoption(
    first,
    postgres,
    network,
    database,
    temporary,
    tls,
    ca,
    passwords,
    owned,
    assignments,
    run,
):
    """PostgreSQL and the static storage root restored onto a new, empty
    cluster: `rebind-cluster` records it, and the controller recreates every
    instance against the restored paths with its original material."""
    admin = Admin(ca, passwords["alice"])
    carol = admin.phase(assignments["carol"]["instanceId"])["namespace"]
    marker = secrets.token_hex(8)
    instance_pod_exec(first, carol, f"echo {marker} > /workspace/e2e-restore")

    # The deployment-owned backup: a database dump and the storage roots.
    dump = subprocess.run(
        ["docker", "exec", postgres, "pg_dump", "-U", "hermes_test", "-Fc", database],
        capture_output=True,
        timeout=300,
        check=True,
    ).stdout
    archive = subprocess.run(
        ["docker", "exec", first, "tar", "-C", "/srv", "-cf", "-", "hermes-data"],
        capture_output=True,
        timeout=300,
        check=True,
    ).stdout
    with Cleanup() as cleanup:
        restored = database + "_restored"
        psql = ["docker", "exec", "-i", postgres]
        cleanup.add(
            "restored database",
            lambda: command(
                [
                    *psql,
                    "psql",
                    "-U",
                    "hermes_test",
                    "-d",
                    "hermes_test",
                    "-c",
                    f"DROP DATABASE IF EXISTS {restored} WITH (FORCE)",
                ]
            ),
        )
        for statement in (
            f"CREATE DATABASE {restored}",
            f"COMMENT ON DATABASE {restored} IS '{database_comment(run)}'",
        ):
            command(
                [
                    *psql,
                    "psql",
                    "-U",
                    "hermes_test",
                    "-d",
                    "hermes_test",
                    "-c",
                    statement,
                ]
            )
        subprocess.run(
            [*psql, "pg_restore", "-U", "hermes_test", "-d", restored, "--no-owner"],
            input=dump,
            capture_output=True,
            timeout=300,
            check=True,
        )
        database_url = (
            f"postgres://hermes_test@{container_ip(postgres, network)}:5432/{restored}"
        )

        second = start_k3s(
            network, "personal-hermes-restore-" + uuid.uuid4().hex[:12], run
        )
        cleanup.add("second cluster", lambda: remove_container(second, run))
        config = wait_ready(second)
        kubeconfig = Path(temporary) / "kubeconfig-restore"
        kubeconfig.write_text(yaml.safe_dump(config))
        kubeconfig.chmod(0o600)
        subprocess.run(
            [
                "docker",
                "exec",
                "-i",
                second,
                "sh",
                "-c",
                "mkdir -p /srv && tar -C /srv -xf -",
            ],
            input=archive,
            capture_output=True,
            timeout=300,
            check=True,
        )
        images = {
            "manager": import_image(second, MANAGER_IMAGE, temporary),
            "hermes": import_image(second, HERMES_IMAGE, temporary),
            "worker": import_image(second, WORKER_TAG, temporary),
        }
        keycloak_image = import_image(second, "hermes-e2e-keycloak:pinned", temporary)
        keycloak(second, keycloak_image, tls, passwords)
        kubectl(
            second,
            "rollout",
            "status",
            "--namespace",
            "keycloak",
            "deployment/keycloak",
            "--timeout=300s",
            timeout=320,
        )
        node = kubectl(
            second, "get", "nodes", "-o", "jsonpath={.items[0].metadata.name}"
        )
        with routed(container_ip(second, network)):
            wait(
                lambda: (
                    requests.get(
                        f"{ISSUER}/.well-known/openid-configuration",
                        verify=ca,
                        timeout=5,
                    ).status_code
                    == 200
                ),
                300,
                "Keycloak did not serve the realm on the new cluster",
            )
            install(
                second,
                kubeconfig,
                temporary,
                images,
                database_url,
                tls,
                node,
                owned,
                RESTORED,
            )
            admin = Admin(ca, passwords["alice"])
            wait(
                lambda: (
                    admin.session.get(f"https://{ADMIN}/healthz", timeout=5).status_code
                    == 200
                ),
                120,
                "the restored administration API is unreachable",
            )
            for name, receipt in assignments.items():
                wait(
                    lambda receipt=receipt: (
                        admin.phase(receipt["instanceId"])["phase"] == "ready"
                    ),
                    READY_TIMEOUT,
                    lambda receipt=receipt, name=name: (
                        f"{name}'s restored instance never became ready: "
                        f"{admin.phase(receipt['instanceId'])}"
                    ),
                    interval=5,
                )
            assert admin.phase(assignments["carol"]["instanceId"])["namespace"] == carol
            kept = instance_pod_exec(second, carol, "cat /workspace/e2e-restore")
            assert kept.strip() == marker, "the restored static volume lost its data"
            browser = Browser(ca, "carol", passwords["carol"])
            assert browser.sign_in().status_code == 200
            assert browser.native_sign_in()["user_id"] == SUBJECTS["carol"]
            an_instance_missing_from_a_restored_backup_is_reported_not_adopted(
                second, postgres, restored, ca, passwords, assignments
            )
    print(
        "PASS: restore onto a new cluster with rebind-cluster; instances recreated "
        "with their material; static volume data back; owner signs in again; an "
        "instance missing from an older backup reported as orphans, not adopted"
    )


def reset(identifier, kubeconfig, postgres, run):
    """Removes an earlier run's installation from a reused fixture cluster.

    Everything removed belongs to that run: both containers are proven to be
    fixtures first, and its databases are the ones it commented, never the ones
    whose names merely look alike.
    """
    require_fixture(identifier, KUBERNETES, run)
    require_fixture(postgres, POSTGRES)
    subprocess.run(
        [
            "helm",
            "uninstall",
            "agents",
            "--namespace",
            NAMESPACE,
            "--kubeconfig",
            str(kubeconfig),
            "--wait",
        ],
        capture_output=True,
        timeout=300,
    )
    # The installation's own policies keep instance namespaces; remove them
    # first if the uninstall left any.
    kubectl(
        identifier,
        "delete",
        "validatingadmissionpolicybindings,validatingadmissionpolicies",
        "--selector",
        "app.kubernetes.io/part-of=hermes-manager",
        "--ignore-not-found",
    )
    namespaces = [
        name
        for name in kubectl(
            identifier, "get", "namespaces", "-o", "jsonpath={.items[*].metadata.name}"
        ).split()
        if name.startswith("hi-") or name in (NAMESPACE, "keycloak")
    ]
    if namespaces:
        # Admission caches see the removed policies only after a moment.
        wait(
            lambda: (
                subprocess.run(
                    [
                        "docker",
                        "exec",
                        identifier,
                        "kubectl",
                        "delete",
                        "namespace",
                        *namespaces,
                        "--ignore-not-found",
                        "--timeout=300s",
                    ],
                    capture_output=True,
                    timeout=320,
                ).returncode
                == 0
            ),
            360,
            "an earlier run's namespaces could not be removed",
            interval=5,
        )
    kubectl(
        identifier,
        "delete",
        "persistentvolumes",
        "--all",
        "--timeout=120s",
        timeout=140,
    )
    command(["docker", "exec", identifier, "rm", "-rf", "/srv/hermes-data"])
    for database in run_databases(postgres, run):
        command(
            [
                "docker",
                "exec",
                postgres,
                "psql",
                "-U",
                "hermes_test",
                "-d",
                "hermes_test",
                "-c",
                f"DROP DATABASE {database} WITH (FORCE)",
            ]
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--postgres-container", default="personal-hermes-manager-tests")
    parser.add_argument(
        "--keep",
        action="store_true",
        help="Keep the run's cluster and database for inspection after a failure",
    )
    parser.add_argument(
        "--reuse", metavar="CONTAINER", help="Reset and reuse a kept fixture cluster"
    )
    args = parser.parse_args()
    # Both containers are injected into and reset: prove they are fixtures.
    require_fixture(args.postgres_container, POSTGRES)
    network = shared_network(args.postgres_container, args.postgres_container)
    with Cleanup() as cleanup:
        if args.reuse:
            identifier = args.reuse
            run = require_fixture(identifier, KUBERNETES)
        else:
            run = new_run()
            identifier = start_k3s(network, "personal-hermes-e2e-" + run, run)

        def release():
            if args.reuse or (cleanup.failed and args.keep):
                print(f"Kept the cluster container {identifier[:12]} for inspection.")
            else:
                remove_container(identifier, run)

        cleanup.add("k3s container", release)
        config = wait_ready(identifier)
        with tempfile.TemporaryDirectory(prefix="personal-hermes-e2e-") as temporary:
            path = Path(temporary) / "kubeconfig"
            path.write_text(yaml.safe_dump(config))
            path.chmod(0o600)
            environment = dict(os.environ)
            environment["HERMES_TEST_KUBECONFIG"] = str(path)
            environment["HERMES_TEST_POSTGRES_CONTAINER"] = args.postgres_container
            environment["HERMES_TEST_RUN"] = run
            if args.keep:
                environment["HERMES_TEST_KEEP"] = "1"
            if args.reuse:
                environment["HERMES_TEST_REUSE"] = "1"
                reset(identifier, path, args.postgres_container, run)
            run_e2e(identifier, temporary, environment)


if __name__ == "__main__":
    main()
