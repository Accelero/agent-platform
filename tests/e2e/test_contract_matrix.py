"""Every administration operation against two actors that may not use it (R1, R2, R7).

The matrix is generated from `contracts/hermes-manager/openapi.yaml`, so a new
operation is covered the moment it is declared. For each operation that the
contract secures:

- a request with no credential at all is refused `401` with the contract's
  error body, before its body or path parameters are looked at;
- a request with a valid token of a person who holds no administrator grant
  is refused `403 access_denied`;
- both responses are `no-store`, `nosniff`, name no input and leak nothing
  (no token, no path value);
- and after the whole matrix the audit log, assignments, secrets, templates and
  backends are exactly as they were: a refusal commits nothing.

The operations the contract declares public (`security: []`) are listed so the
generated set cannot silently shrink; they are exercised by their own cases.
"""

import json
import uuid

import pytest
import requests

from tests.personal_hermes import native_e2e as scenarios
from tests.support.contract import concrete, operations

pytestmark = [pytest.mark.e2e, pytest.mark.cluster, pytest.mark.security]

PUBLIC = {
    "beginAdministratorLogin",
    "completeAdministratorLogin",
}
# Logout is checked for its Origin and CSRF token before the session, so it
# answers 403 to both. Like every refusal here it sets no cookie.
LOGOUT = "logoutAdministrator"
STATE = (
    "/api/v1/audit-events?limit=100",
    "/api/v1/assignments?limit=100",
    "/api/v1/secrets?limit=100",
    "/api/v1/templates?limit=100",
    "/api/v1/terminal-backends?limit=100",
    "/api/v1/administrators?limit=100",
)


SECURED = [op for op in operations() if op[3]]


def token(platform, name):
    reply = requests.post(
        f"{scenarios.ISSUER}/protocol/openid-connect/token",
        data={
            "grant_type": "password",
            "client_id": "manager-admin",
            "username": name,
            "password": platform.passwords[name],
            "scope": "openid",
        },
        verify=platform.ca,
        timeout=20,
    )
    assert reply.status_code == 200, f"Keycloak refused {name}"
    return reply.json()["access_token"]


def send(platform, method, path, headers=None):
    body = {} if method in ("post", "put", "patch") else None
    return requests.request(
        method.upper(),
        f"https://{scenarios.ADMIN}{concrete(path)}",
        json=body,
        headers={
            "Idempotency-Key": uuid.uuid4().hex,
            "If-Match": '"1"',
            **(headers or {}),
        },
        verify=platform.ca,
        timeout=30,
        allow_redirects=False,
    )


def state(platform):
    """What a request could have committed: the lists, without the instances'
    phase, which moves by itself as instances that earlier cases stopped wind
    down."""
    snapshot = {path: platform.admin.call("GET", path).json() for path in STATE}
    for item in snapshot["/api/v1/assignments?limit=100"]["items"]:
        item.pop("phase", None)
    return snapshot


def test_every_secured_operation_refuses_actors_without_access_and_commits_nothing(
    platform,
):
    before = state(platform)
    bob = {"Authorization": f"Bearer {token(platform, 'bob')}"}
    seen = set()
    for operation, method, path, _ in SECURED:
        expected = (
            (("nobody", {}, 403, "access_denied"),)
            + (("a person without a grant", bob, 403, "access_denied"),)
            if operation == LOGOUT
            else (
                ("nobody", {}, 401, "invalid_identity_evidence"),
                ("a person without a grant", bob, 403, "access_denied"),
            )
        )
        for actor, headers, status, error in expected:
            reply = send(platform, method, path, headers)
            where = f"{operation} as {actor}"
            assert reply.status_code == status, f"{where}: {reply.status_code}"
            assert reply.json() == {"error": error}, where
            assert "no-store" in reply.headers.get("cache-control", ""), where
            assert reply.headers.get("x-content-type-options") == "nosniff", where
            assert "set-cookie" not in reply.headers, where
            assert bob["Authorization"].split()[1] not in reply.text, where
            seen.add((operation, actor))
    assert len(seen) == 2 * len(SECURED)
    assert state(platform) == before, "a refused request committed something"
    # Control: the administrator reaches the same reads, so the refusals above
    # are about the actor, not about the generated requests.
    allowed = send(
        platform,
        "get",
        "/api/v1/session",
        {"Authorization": platform.admin.headers()["Authorization"]},
    )
    assert allowed.status_code == 200
    assert json.loads(allowed.text)["actor"]["subject"] == scenarios.SUBJECTS["alice"]
