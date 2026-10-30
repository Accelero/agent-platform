"""Every transition between the three access states, on one instance (R4, R9).

`enabled`, `suspended` and `revoked` differ only in name for the instance: a
person who is not enabled is denied at the gateway, nothing of their instance
runs, and everything they own is kept. Enabling again restarts the same
instance on the same data with the same credentials. The case walks all six
ordered transitions, so the pairs that no scenario exercised (revoked to
suspended, revoked back to enabled, enabled straight to revoked) are covered.

It makes its own person and instance on the shared platform.
"""

import secrets
import time

import pytest

from tests.personal_hermes import native_e2e as scenarios

pytestmark = [pytest.mark.e2e, pytest.mark.cluster, pytest.mark.slow]

# Every ordered pair of distinct states, in a walk that visits each once.
WALK = [
    "suspended",  # enabled -> suspended
    "revoked",  # suspended -> revoked
    "enabled",  # revoked -> enabled
    "revoked",  # enabled -> revoked
    "suspended",  # revoked -> suspended
    "enabled",  # suspended -> enabled
]


def pods(platform, namespace):
    return scenarios.kubectl(
        platform.identifier, "get", "pods", "--namespace", namespace, "-o", "name"
    ).split()


def claims(platform, namespace):
    return scenarios.kubectl(
        platform.identifier,
        "get",
        "pvc",
        "--namespace",
        namespace,
        "-o",
        "jsonpath={.items[*].metadata.uid}",
    )


def served(owner, step):
    """Polls the owner's native session until it answers 200; returns every
    status seen on the way."""
    statuses = set()

    def back():
        reply = owner.get("/api/auth/me")
        statuses.add(reply.status_code)
        return reply.status_code == 200

    scenarios.wait(back, scenarios.READY_TIMEOUT, f"{step}: not back", interval=5)
    return statuses


def test_every_access_state_denies_the_owner_and_keeps_the_instance_and_its_data(
    platform,
):
    admin = platform.admin
    username = f"states-{secrets.token_hex(3)}"
    password = secrets.token_urlsafe(18)
    directory = scenarios.Directory(platform.ca, platform.passwords["directory"])
    subject = directory.create(username, f"{username}@e2e.test", password)
    receipt = admin.call(
        "POST",
        "/api/v1/assignments",
        {
            "identity": {"issuer": scenarios.ISSUER, "subject": subject},
            "spec": {
                "access": "enabled",
                "template": {"id": "personal", "version": platform.state.template},
            },
        },
    ).json()
    instance, assignment = receipt["instanceId"], receipt["assignmentId"]

    def ready():
        scenarios.wait(
            lambda: admin.phase(instance)["phase"] == "ready",
            scenarios.READY_TIMEOUT,
            lambda: admin.phase(instance),
            interval=5,
        )

    ready()
    namespace = admin.phase(instance)["namespace"]
    owner = scenarios.Browser(platform.ca, username, password)
    assert owner.sign_in().status_code == 200
    owner.native_sign_in()

    marker = secrets.token_hex(8)
    scenarios.instance_pod_exec(
        platform.identifier, namespace, f"echo {marker} > /opt/data/e2e-marker"
    )
    volumes = claims(platform, namespace)
    assert volumes
    credentials = scenarios.secret_digests(
        platform.identifier, namespace, "hermes-credentials"
    )
    state = "enabled"

    for target in WALK:
        step = f"{state} -> {target}"
        was_running = state == "enabled"
        connection = owner.websocket() if was_running else None
        admin.set_access(assignment, target)
        if target == "enabled":
            statuses = served(owner, step)
            ready()
            assert statuses <= {200, 202, 403}, f"{step}: unexpected {statuses}"
            assert admin.phase(instance)["id"] == instance, step
            kept = scenarios.instance_pod_exec(
                platform.identifier, namespace, "cat /opt/data/e2e-marker"
            )
            assert kept.strip() == marker, f"{step}: the home volume was replaced"
            assert claims(platform, namespace) == volumes, step
            assert (
                scenarios.secret_digests(
                    platform.identifier, namespace, "hermes-credentials"
                )
                == credentials
            ), f"{step}: credentials were regenerated"
        else:
            if connection is not None:
                took = scenarios.closed_within(connection, 15)
                assert took < 12, f"{step}: an open WebSocket outlived the change"
            # Denied within the cache bound, and never let through again.
            deadline = time.monotonic() + 15
            while owner.get("/api/auth/me").status_code != 403:
                assert time.monotonic() < deadline, f"{step}: not denied"
                time.sleep(0.5)
            page = owner.get("/", headers={"Accept": "text/html"})
            assert page.status_code == 403, f"{step}: {page.status_code}"
            scenarios.wait(
                lambda: admin.phase(instance)["phase"] == "retained",
                300,
                f"{step}: the instance was not retained",
                interval=5,
            )
            assert not pods(platform, namespace), f"{step}: Pods left running"
            for _ in range(5):
                assert owner.get("/api/auth/me").status_code == 403, step
                time.sleep(1)
            assert admin.phase(instance)["id"] == instance, step
            assert claims(platform, namespace) == volumes, step
        state = target
