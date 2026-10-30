"""Every administration operation addressed at something that does not exist (R1, R7).

Generated from `contracts/hermes-manager/openapi.yaml`: each operation that
takes a path parameter, as the administrator, with a well-formed identifier that
names nothing and with a malformed one. The answer is the same in both: `404
not_found`, private and inert, naming nothing the client sent, and committing
nothing (the audit log, assignments, secrets, catalogs and administrators are
exactly as before). A malformed identifier is not told apart from a missing
one, so the response cannot be used to probe what exists.

The two listings of a catalog's versions are collections: for a well-formed name
that has no versions, which is every name nothing was ever drafted under, the
contract's answer is an empty page, not `404`.

The operations that create something under a new name (the two draft creations
and `writeSecret`) have no "unknown" case and are left out.
"""

import uuid

import pytest

from tests.personal_hermes import native_e2e as scenarios
from tests.support.contract import operations
from tests.support.people import template

pytestmark = [pytest.mark.e2e, pytest.mark.cluster, pytest.mark.security]

CREATES = {"createTemplateDraft", "createTerminalBackendDraft", "writeSecret"}
COLLECTIONS = {"listTemplateVersions", "listTerminalBackendVersions"}
STATE = (
    "/api/v1/audit-events?limit=100",
    "/api/v1/assignments?limit=100",
    "/api/v1/secrets?limit=100",
    "/api/v1/templates?limit=100",
    "/api/v1/terminal-backends?limit=100",
    "/api/v1/administrators?limit=100",
)
ADDRESSED = [
    (operation, method, path)
    for operation, method, path, _ in operations()
    if "{" in path and operation not in CREATES
]


def backend(platform):
    return {
        **platform.state.worker,
        "limits": {"idleSeconds": 300, "maxLeaseSeconds": 900, "maxEnvironments": 2},
        "mounts": {"workspace": {"mountPath": "/workspace"}},
    }


def body(operation, platform):
    """The smallest body the operation accepts, so that the lookup is what
    fails and not the validation before it."""
    return {
        "updateAssignment": {"access": "enabled"},
        "editTemplateDraft": {"definition": template(platform)},
        "editTerminalBackendDraft": {"definition": backend(platform)},
        "updateAdministrator": {"active": True, "canManageAdministrators": False},
    }.get(operation, {} if operation != "deleteSecret" else None)


def filled(path, malformed):
    values = {
        "identityId": ("not-a-uuid", str(uuid.uuid4())),
        "assignmentId": ("not-a-uuid", str(uuid.uuid4())),
        "instanceId": ("not-a-uuid", str(uuid.uuid4())),
        "eventId": ("not-a-uuid", str(uuid.uuid4())),
        "catalogId": ("Bad_Name", "no-such-name"),
        "version": ("zero", "7"),
        "secretName": ("Bad_Name", "no-such-secret"),
    }
    for name, (bad, good) in values.items():
        path = path.replace("{" + name + "}", bad if malformed else good)
    assert "{" not in path, path
    return path


def state(platform):
    """What a request could have committed: the lists, without the instances'
    phase, which moves by itself as instances that earlier cases stopped wind
    down."""
    snapshot = {path: platform.admin.call("GET", path).json() for path in STATE}
    for item in snapshot["/api/v1/assignments?limit=100"]["items"]:
        item.pop("phase", None)
    return snapshot


def test_the_set_of_addressed_operations_is_the_contracts():
    assert len(ADDRESSED) >= 21, ADDRESSED


@pytest.mark.parametrize("malformed", [False, True], ids=["unknown", "malformed"])
@pytest.mark.parametrize(
    "operation, method, path", ADDRESSED, ids=[op for op, _, _ in ADDRESSED]
)
def test_an_identifier_that_names_nothing_is_not_found_and_changes_nothing(
    platform, operation, method, path, malformed
):
    concrete = filled(path, malformed)
    before = state(platform)
    reply = platform.admin.session.request(
        method.upper(),
        f"https://{scenarios.ADMIN}{concrete}",
        json=body(operation, platform),
        headers=platform.admin.headers(etag='"1"'),
        timeout=30,
    )
    if operation in COLLECTIONS and not malformed:
        assert reply.status_code == 200, (operation, reply.status_code)
        assert reply.json() == {"items": [], "nextCursor": None}
    else:
        assert reply.status_code == 404, (
            operation,
            reply.status_code,
            reply.text[:120],
        )
        assert reply.json() == {"error": "not_found"}
    assert "no-store" in reply.headers["cache-control"]
    assert reply.headers["x-content-type-options"] == "nosniff"
    for sent in concrete.strip("/").split("/")[3:]:
        if len(sent) > 8:
            assert sent not in reply.text, "the answer echoes the identifier"
    assert state(platform) == before, f"{operation} committed something"


def test_a_known_identifier_is_found_by_the_same_requests(platform):
    """The control: the same reads reach what exists, so the refusals above are
    about the identifier and not about the generated requests."""
    admin = platform.admin
    assignment = next(iter(platform.assignments.values()))
    assert admin.call("GET", f"/api/v1/assignments/{assignment['assignmentId']}")
    assert admin.call("GET", f"/api/v1/instances/{assignment['instanceId']}")
    assert admin.call(
        "GET", f"/api/v1/templates/personal/versions/{platform.state.template}"
    )
    assert admin.call(
        "GET", f"/api/v1/instances/{assignment['instanceId']}/environments"
    )
