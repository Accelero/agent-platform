"""Stale, missing and conflicting state in the administration API (R1, R2, R7).

The specification: an update carries the version it was based on as `If-Match`;
one without it is `422` (incomplete), one with another version is `412`
(stale), a state conflict is `409`, and a refused request changes nothing. The
contract declares which operations can answer `412` and names the conflicts.
Each case makes its own objects on the shared platform and shows, for every
operation that declares `412`, that

- a stale precondition and a missing one are refused with their own status and
  code, privately, and change nothing;
- the same request with the current version goes through (the control, so the
  refusals are about the precondition and not about the generated request).

The named conflicts follow: an identity with an assignment, a second grant, the
last manager, a referenced secret, a version in the wrong state, and an
idempotency key reused with another body, while the same key sent to another
command is a separate change.
"""

import secrets
import uuid

import pytest
import yaml

from tests.personal_hermes import native_e2e as scenarios
from tests.support.contract import CONTRACT, METHODS
from tests.support.people import person, template

pytestmark = [pytest.mark.e2e, pytest.mark.cluster, pytest.mark.security]

STALE = '"999999"'


def backend(platform):
    return {
        **platform.state.worker,
        "limits": {"idleSeconds": 300, "maxLeaseSeconds": 900, "maxEnvironments": 2},
        "mounts": {"workspace": {"mountPath": "/workspace"}},
    }


def definition_of(platform, kind):
    return template(platform) if kind == "templates" else backend(platform)


def altered(platform, kind):
    """The kind's definition with one valid field changed."""
    definition = definition_of(platform, kind)
    if kind == "templates":
        return {**definition, "resources": {"cpuMillis": 1500, "memoryMib": 3072}}
    return {**definition, "limits": {**definition["limits"], "idleSeconds": 120}}


def send(platform, method, path, body=None, etag=None, key=None):
    """One administration request: no `etag` sends no If-Match at all, and a
    `key` replaces the fresh Idempotency-Key."""
    headers = platform.admin.headers(etag)
    if key is not None:
        headers["Idempotency-Key"] = key
    return platform.admin.session.request(
        method,
        f"https://{scenarios.ADMIN}{path}",
        json=body,
        headers=headers,
        timeout=30,
    )


def read(platform, path):
    reply = send(platform, "GET", path)
    assert reply.status_code == 200, (path, reply.status_code, reply.text[:200])
    return reply


def new_draft(platform, kind, tag):
    reply = send(
        platform,
        "POST",
        f"/api/v1/{kind}/prec-{tag}/versions",
        {"definition": definition_of(platform, kind)},
    )
    assert reply.status_code == 201, reply.text
    return f"/api/v1/{kind}/prec-{tag}/versions/{reply.json()['version']}"


def new_published(platform, kind, tag):
    path = new_draft(platform, kind, tag)
    done = send(
        platform, "POST", f"{path}/publish", etag=read(platform, path).headers["ETag"]
    )
    assert done.status_code == 200, done.text
    return path


def version_state(platform, path):
    """A catalog version as the administrator reads it, without the clock."""
    reply = read(platform, path).json()
    reply.pop("updatedAt", None)
    return reply


class Case:
    """One request, what it would commit, where its current version is read
    (`etag`), and whether the request with that version changes anything
    visible."""

    def __init__(self, request, body, state, etag, visible=True):
        self.request, self.body, self.state = request, body, state
        self.etag, self.visible = etag, visible


def assignment_case(platform, tag, retry):
    """A suspended assignment, so that no Pods start."""
    receipt = person(
        platform,
        "prec",
        {
            "access": "suspended",
            "template": {"id": "personal", "version": platform.state.template},
        },
    )
    path = f"/api/v1/assignments/{receipt['assignmentId']}"

    def state():
        item = read(platform, path).json()
        return item["version"], item["spec"]

    etag = lambda: read(platform, path).headers["ETag"]  # noqa: E731
    if retry:
        return Case(("POST", f"{path}/retry"), None, state, etag, visible=False)
    return Case(
        ("PATCH", path), {"adminConfig": {"display": {"compact": True}}}, state, etag
    )


def draft_case(platform, tag, kind, publish):
    path = new_draft(platform, kind, tag)

    def state():
        return version_state(platform, path)

    def etag():
        return read(platform, path).headers["ETag"]

    if publish:
        return Case(("POST", f"{path}/publish"), None, state, etag)
    return Case(("PATCH", path), {"definition": altered(platform, kind)}, state, etag)


def withdraw_case(platform, tag, kind):
    path = new_published(platform, kind, tag)
    return Case(
        ("POST", f"{path}/withdraw"),
        None,
        lambda: version_state(platform, path),
        lambda: read(platform, path).headers["ETag"],
    )


def secret_case(platform, tag):
    name = f"prec-{tag}"
    platform.admin.call("PUT", f"/api/v1/secrets/{name}", {"kind": "env", "value": tag})

    def state():
        listed = platform.admin.call("GET", "/api/v1/secrets?limit=100").json()["items"]
        return [
            {k: v for k, v in item.items() if k != "updatedAt"}
            for item in listed
            if item["name"] == name
        ]

    def etag():
        (item,) = state()
        return f'"{item["version"]}"'

    return Case(("DELETE", f"/api/v1/secrets/{name}"), None, state, etag)


def manager(platform):
    """The commissioned administrator, the only one who manages the others."""
    listed = platform.admin.call("GET", "/api/v1/administrators?limit=100").json()
    (alice,) = [
        item
        for item in listed["items"]
        if item["identity"]["subject"] == scenarios.SUBJECTS["alice"]
    ]
    return alice


def administrator_case(platform, tag):
    """Alice's own record, rewritten with the values it has: the manager is
    never left without the right, and the version still moves."""
    alice = manager(platform)
    path = f"/api/v1/administrators/{alice['identityId']}"
    body = {"active": True, "canManageAdministrators": True}

    def state():
        item = read(platform, path).json()
        return item["version"], item["active"], item["canManageAdministrators"]

    return Case(
        ("PATCH", path), body, state, lambda: read(platform, path).headers["ETag"]
    )


OPERATIONS = {
    "updateAssignment": lambda p, t: assignment_case(p, t, retry=False),
    "retryAssignment": lambda p, t: assignment_case(p, t, retry=True),
    "editTemplateDraft": lambda p, t: draft_case(p, t, "templates", publish=False),
    "publishTemplate": lambda p, t: draft_case(p, t, "templates", publish=True),
    "withdrawTemplate": lambda p, t: withdraw_case(p, t, "templates"),
    "editTerminalBackendDraft": lambda p, t: draft_case(
        p, t, "terminal-backends", publish=False
    ),
    "publishTerminalBackend": lambda p, t: draft_case(
        p, t, "terminal-backends", publish=True
    ),
    "withdrawTerminalBackend": lambda p, t: withdraw_case(p, t, "terminal-backends"),
    "deleteSecret": secret_case,
    "updateAdministrator": administrator_case,
}


def test_every_operation_that_declares_412_is_covered():
    spec = yaml.safe_load(CONTRACT.read_text())
    declared = {
        item[method]["operationId"]
        for item in spec["paths"].values()
        for method in METHODS
        if method in item and "412" in item[method]["responses"]
    }
    assert declared == set(OPERATIONS), declared ^ set(OPERATIONS)
    # The contract says what a missing If-Match gets: 422 is declared wherever
    # 412 is.
    assert all(
        "422" in item[method]["responses"]
        for item in spec["paths"].values()
        for method in METHODS
        if method in item and "412" in item[method]["responses"]
    )


@pytest.mark.parametrize("precondition", ["stale", "missing"])
@pytest.mark.parametrize("operation", OPERATIONS)
def test_a_stale_or_missing_precondition_is_refused_and_changes_nothing(
    platform, operation, precondition
):
    case = OPERATIONS[operation](platform, secrets.token_hex(3))
    method, path = case.request
    before = case.state()
    refused = send(
        platform,
        method,
        path,
        case.body,
        etag=STALE if precondition == "stale" else None,
    )
    expected = (
        (412, "stale_version") if precondition == "stale" else (422, "invalid_request")
    )
    assert (refused.status_code, refused.json()) == (
        expected[0],
        {"error": expected[1]},
    ), refused.text
    assert "no-store" in refused.headers["cache-control"]
    assert case.state() == before, f"{operation} changed something"
    # The control: the current version is what lets the same request through.
    done = send(platform, method, path, case.body, etag=case.etag())
    assert done.status_code in (200, 202), (operation, done.status_code, done.text)
    if case.visible:
        assert case.state() != before, f"{operation} did nothing"


def test_an_identity_holds_one_assignment_and_a_grant_exists_once(platform):
    receipt = person(
        platform,
        "prec",
        {
            "access": "suspended",
            "template": {"id": "personal", "version": platform.state.template},
        },
    )
    current = read(platform, f"/api/v1/assignments/{receipt['assignmentId']}").json()
    listed = platform.admin.call("GET", "/api/v1/assignments?limit=100").json()["items"]
    again = send(
        platform,
        "POST",
        "/api/v1/assignments",
        {"identity": current["identity"], "spec": current["spec"]},
    )
    assert (again.status_code, again.json()) == (409, {"error": "assignment_exists"})
    after = platform.admin.call("GET", "/api/v1/assignments?limit=100").json()["items"]
    assert [item["id"] for item in after] == [item["id"] for item in listed]

    alice = manager(platform)
    regrant = send(
        platform,
        "POST",
        "/api/v1/administrators",
        {"identity": alice["identity"], "canManageAdministrators": True},
    )
    assert (regrant.status_code, regrant.json()) == (
        409,
        {"error": "administrator_grant_exists"},
    )
    assert manager(platform) == alice


def test_the_last_manager_and_a_referenced_secret_cannot_be_removed(platform):
    alice = manager(platform)
    path = f"/api/v1/administrators/{alice['identityId']}"
    for change in (
        {"active": False, "canManageAdministrators": True},
        {"active": True, "canManageAdministrators": False},
        {"active": False, "canManageAdministrators": False},
    ):
        etag = read(platform, path).headers["ETag"]
        refused = send(platform, "PATCH", path, change, etag=etag)
        assert (refused.status_code, refused.json()) == (
            409,
            {"error": "last_administrator"},
        ), change
        assert manager(platform) == alice, change

    listed = platform.admin.call("GET", "/api/v1/secrets?limit=100").json()["items"]
    (key,) = [item for item in listed if item["name"] == "llm-key"]
    assert key["references"] >= 1
    refused = send(
        platform, "DELETE", "/api/v1/secrets/llm-key", etag=f'"{key["version"]}"'
    )
    assert (refused.status_code, refused.json()) == (409, {"error": "secret_in_use"})
    kept = platform.admin.call("GET", "/api/v1/secrets?limit=100").json()["items"]
    assert [item for item in kept if item["name"] == "llm-key"] == [key]


@pytest.mark.parametrize("kind", ["templates", "terminal-backends"])
def test_a_version_moves_only_from_draft_to_published_to_withdrawn(platform, kind):
    path = new_draft(platform, kind, secrets.token_hex(3))

    def etag():
        return read(platform, path).headers["ETag"]

    def refused(method, suffix="", body=None):
        before = version_state(platform, path)
        reply = send(platform, method, f"{path}{suffix}", body, etag=etag())
        assert (reply.status_code, reply.json()) == (
            409,
            {"error": "version_not_draft"},
        ), (
            method,
            suffix,
            reply.text,
        )
        assert version_state(platform, path) == before, (method, suffix)

    # A draft cannot be withdrawn.
    refused("POST", "/withdraw")
    assert send(platform, "POST", f"{path}/publish", etag=etag()).status_code == 200
    assert version_state(platform, path)["state"] == "published"
    # A published version is immutable and cannot be published again.
    refused("PATCH", body={"definition": altered(platform, kind)})
    refused("POST", "/publish")
    assert send(platform, "POST", f"{path}/withdraw", etag=etag()).status_code == 200
    assert version_state(platform, path)["state"] == "withdrawn"
    # A withdrawn one is final.
    refused("PATCH", body={"definition": altered(platform, kind)})
    refused("POST", "/publish")
    refused("POST", "/withdraw")


def test_an_idempotency_key_repeats_its_command_and_refuses_another_body(platform):
    tag = secrets.token_hex(3)
    key = uuid.uuid4().hex
    path = f"/api/v1/templates/prec-{tag}/versions"
    definition = template(platform)

    def versions():
        return read(platform, path).json()["items"]

    first = send(platform, "POST", path, {"definition": definition}, key=key)
    assert first.status_code == 201
    # The same command with the same body is the same change: the recorded
    # result, and no second draft.
    repeated = send(platform, "POST", path, {"definition": definition}, key=key)
    assert (repeated.status_code, repeated.json()) == (201, first.json())
    assert len(versions()) == 1, "a repeated command made another draft"
    # Another body under the same key is a conflict that changes nothing.
    other = {
        "definition": {
            **definition,
            "resources": {"cpuMillis": 1500, "memoryMib": 3072},
        }
    }
    conflict = send(platform, "POST", path, other, key=key)
    assert (conflict.status_code, conflict.json()) == (
        409,
        {"error": "idempotency_conflict"},
    )
    assert len(versions()) == 1
    # The key is scoped to the command: another command may use it, each as
    # its own change.
    name = f"/api/v1/secrets/prec-{tag}"
    written = send(platform, "PUT", name, {"kind": "env", "value": tag}, key=key)
    assert written.status_code == 200, written.text
    removed = send(platform, "DELETE", name, etag='"1"', key=key)
    assert removed.status_code == 200, removed.text
