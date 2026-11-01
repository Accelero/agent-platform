"""The TLS API as a client sees it: contract, error model and transport (X9, X10,
X11, R7).
"""

import socket
import time
from copy import deepcopy

import pytest
from runtime_manager import server as transport

from tests.support import contract
from tests.support.http import serve
from tests.support.world import CUSTOM_IMAGE, DEFAULT_IMAGE, default_config, make_world

pytestmark = pytest.mark.integration

ACQUIRE = {"profile": "default", "session": "s", "mode": "per_profile_on_demand"}


def expect(http, method, path, status, code=None, body=None, **options):
    """Call, check the response against the contract and return its body."""
    got, headers, payload = http.call(method, path, body, **options)
    assert got == status, (got, payload)
    contract.check(method, path, got, payload, headers)
    if code is not None:
        assert payload["code"] == code, payload
    return payload


def test_every_operation_of_the_contract_is_served_and_conforms(world, tls):
    """X9: each documented operation answers with a body its schema accepts."""
    seen = set()

    def call(method, path, status, body=None, **options):
        seen.add((method, path))
        return expect(http, method, path, status, body=body, **options)

    with serve(world, tls) as http:
        call("GET", "/healthz", 200, token="none")
        lease = call("POST", "/v1/leases/acquire", 200, {**ACQUIRE, "request": "r1"})
        call("POST", "/v1/leases/heartbeat", 200, {"lease": lease["lease"]})
        assert call("GET", "/v1/environments", 200)[0]["phase"] == "ready"
        call("GET", "/v1/orphans", 200)
        call("POST", "/v1/leases/release", 200, {"lease": lease["lease"]})
        call("POST", "/v1/sessions/close", 200, {"profile": "default", "session": "s"})
    assert seen == set(contract.operations())


def test_orphans_are_reported_over_the_api(world, tls):
    """X11."""
    stray = "hermes-exec-" + "c" * 24 + "-" + "d" * 12
    world.kube.add(
        {
            "metadata": {
                "name": stray,
                "labels": {
                    "app.kubernetes.io/managed-by": "agent-platform-runtime-manager"
                },
            },
            "spec": {"containers": [{"image": DEFAULT_IMAGE}]},
        },
        created=world.clock() - 1000,
    )
    world.service.scan_orphans()
    with serve(world, tls) as http:
        (orphan,) = expect(http, "GET", "/v1/orphans", 200)
    assert orphan["name"] == stray


def drifted(world):
    world.config["templates"]["terminal"]["kubernetes"]["resources"] = {
        "limits": {"cpu": "2"}
    }


def scenario_pending(world):
    world.kube.default_boot = 100


def scenario_draining(world):
    world.acquire(request="held")
    drifted(world)
    world.service.maintain()


def scenario_ended(world):
    world.post("/v1/leases/release", lease=world.acquire(request="r1")["lease"])


def scenario_leased(world):
    world.acquire(request="held")


def scenario_failed(world):
    world.kube.pull_errors = {DEFAULT_IMAGE}


def scenario_broken_cluster(world):
    def broken(*args, body=None):
        raise RuntimeError("Kubernetes execution operation failed: unavailable")

    world.cluster.kubectl = broken


ERRORS = [
    (
        "pending",
        scenario_pending,
        "/v1/leases/acquire",
        ACQUIRE,
        409,
        "environment_pending",
    ),
    (
        "draining",
        scenario_draining,
        "/v1/leases/acquire",
        {**ACQUIRE, "request": "new"},
        409,
        "environment_draining",
    ),
    (
        "ended",
        scenario_ended,
        "/v1/leases/acquire",
        {**ACQUIRE, "request": "r1"},
        409,
        "lease_not_live",
    ),
    (
        "expired",
        lambda w: None,
        "/v1/leases/heartbeat",
        {"lease": "0" * 32},
        409,
        "lease_expired",
    ),
    (
        "changed",
        scenario_leased,
        "/v1/leases/acquire",
        {**ACQUIRE, "request": "new", "image": CUSTOM_IMAGE},
        409,
        "conflict",
    ),
    ("failed", scenario_failed, "/v1/leases/acquire", ACQUIRE, 422, "backend_failed"),
    (
        "cluster",
        scenario_broken_cluster,
        "/v1/leases/acquire",
        ACQUIRE,
        503,
        "unavailable",
    ),
    (
        "template",
        lambda w: None,
        "/v1/leases/acquire",
        {**ACQUIRE, "template": "missing"},
        400,
        "invalid_request",
    ),
    (
        "fields",
        lambda w: None,
        "/v1/leases/acquire",
        {**ACQUIRE, "unexpected": 1},
        400,
        "invalid_request",
    ),
    (
        "identifier",
        lambda w: None,
        "/v1/leases/release",
        {"lease": "bad id!"},
        400,
        "invalid_request",
    ),
]


@pytest.mark.parametrize(
    ("scenario", "setup", "path", "body", "status", "code"),
    ERRORS,
    ids=[case[0] for case in ERRORS],
)
def test_every_error_carries_a_stable_code_that_the_contract_declares(
    world, tls, scenario, setup, path, body, status, code
):
    """X9."""
    setup(world)
    body = {"request": "r0", **body} if path.endswith("acquire") else body
    with serve(world, tls) as http:
        got, headers, payload = http.call("POST", path, body)
        assert (got, payload["code"]) == (status, code), payload
        contract.check("POST", path, got, payload, headers)
        if code == "backend_failed":
            assert payload["reason"] == "image_unavailable"
        if code in ("environment_pending", "environment_draining"):
            assert headers["Retry-After"] == "1"


def test_another_clients_lease_is_forbidden(tmp_path, tls):
    """R7: the second client has its own credential and deployment."""
    config = default_config()
    config["clients"]["codex"] = {"deployment": "other"}
    config["deployments"]["other"] = {}
    world = make_world(tmp_path, config=config)
    try:
        lease = world.acquire()["lease"]
        with serve(world, tls, client="codex") as http:
            body = expect(
                http, "POST", "/v1/leases/heartbeat", 403, "forbidden", {"lease": lease}
            )
            assert "another client" in body["error"]
    finally:
        world.close()


def test_requests_are_validated_before_they_reach_the_manager(world, tls):
    """X9: strict JSON, bounded bodies, authentication on everything but /healthz."""
    with serve(world, tls) as http:
        for token in ("wrong", "x" * 40):
            status, _, body = http.call("GET", "/v1/environments", token=token)
            assert (status, body["code"]) == (401, "unauthorized")
        status, _, body = http.call(
            "GET", "/v1/environments", headers={"Authorization": "Basic abc"}
        )
        assert status == 401
        duplicate = b'{"lease": "a", "lease": "b"}'
        for raw, headers in (
            (duplicate, {}),
            (b"not json", {}),
            (b'{"lease": "a"}', {"Content-Type": "text/plain"}),
            (b'{"lease": "a"}', {"Content-Length": "99999"}),
            (b"", {}),
        ):
            status, _, body = http.call(
                "POST", "/v1/leases/release", raw=raw, headers=headers
            )
            assert (status, body["code"]) == (400, "invalid_request"), raw
        status, _, body = http.call("GET", "/v1/unknown")
        assert (status, body["code"]) == (404, "not_found")
        status, _, body = http.call("POST", "/v1/unknown", {"a": 1})
        assert (status, body["code"]) == (404, "not_found")


def test_a_connection_that_never_finishes_its_handshake_blocks_nobody(
    world, tls, monkeypatch
):
    """X10: one silent client used to stall every other for the whole socket timeout."""
    monkeypatch.setattr(transport, "SOCKET_TIMEOUT", 0.5)
    with serve(world, tls) as http:
        silent = socket.create_connection(("127.0.0.1", http.port))
        try:
            started = time.monotonic()
            expect(http, "GET", "/healthz", 200, token="none")
            assert time.monotonic() - started < 0.4
            silent.settimeout(5)
            assert silent.recv(1) == b""  # the server gives up on it and closes
            assert time.monotonic() - started < 3
        finally:
            silent.close()


def test_the_configuration_of_one_template_does_not_leak_into_responses(world, tls):
    """R9: a response carries the SSH endpoint, nothing from the template or the
    cluster.
    """
    world.config["templates"]["terminal"]["kubernetes"]["environment"] = {
        "SECRET_LOOKING": "value"
    }
    with serve(world, tls) as http:
        lease = expect(
            http, "POST", "/v1/leases/acquire", 200, body={**ACQUIRE, "request": "r1"}
        )
    assert "SECRET_LOOKING" not in str(lease) and set(lease["ssh"]) == {
        "host",
        "port",
        "user",
    }
    snapshot = deepcopy(lease)
    assert snapshot == lease
