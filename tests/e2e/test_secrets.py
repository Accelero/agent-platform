"""Admin secret rotation on a platform of its own (R11).

Bob's template references the model key; Carol's does not. A new version of the
secret must restart Bob's Hermes only, deliver the new value to it and nowhere
else, and neither value may ever be returned by the API or recorded in the
audit log. The case rotates the key twice and leaves the original in place.
"""

import base64
import json
import secrets
import time

import pytest
from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey
from cryptography.hazmat.primitives.serialization import (
    Encoding,
    NoEncryption,
    PrivateFormat,
)

from tests.personal_hermes import native_e2e as scenarios

pytestmark = [
    pytest.mark.e2e,
    pytest.mark.cluster,
    pytest.mark.destructive,
    pytest.mark.slow,
    pytest.mark.security,
]

KEY = "OPENAI_API_KEY"


def delivered(platform, namespace):
    """The variable as the instance's own Hermes sees it, or `ABSENT`."""
    return scenarios.instance_pod_exec(
        platform.identifier, namespace, f"printenv {KEY} || echo ABSENT"
    ).strip()


def restarted(platform, instance, namespace, before):
    """Waits for a new Hermes Pod, then for the instance to be ready again."""

    def replaced():
        try:
            return scenarios.hermes_pod(platform.identifier, namespace) != before
        except AssertionError:
            return False  # the old Pod is gone and the new one not yet created

    scenarios.wait(replaced, 600, "the instance never restarted", interval=5)
    scenarios.wait(
        lambda: platform.admin.phase(instance)["phase"] == "ready",
        600,
        "the restarted instance never became ready",
        interval=5,
    )


def test_rotating_an_admin_secret_restarts_only_its_instances_and_never_returns_it(
    destructive_platform,
):
    platform = destructive_platform
    admin, original = platform.admin, platform.passwords["llm"]
    bob = platform.assignments["bob"]["instanceId"]
    carol = platform.assignments["carol"]["instanceId"]
    bob_space = admin.phase(bob)["namespace"]
    carol_space = admin.phase(carol)["namespace"]

    assert delivered(platform, bob_space) == original
    assert delivered(platform, carol_space) == "ABSENT", "an unreferencing instance"
    carol_pod = scenarios.hermes_pod(platform.identifier, carol_space)

    # A referenced secret cannot be deleted, and the refusal removes nothing.
    admin.call("DELETE", "/api/v1/secrets/llm-key", expected=(409, 422))
    assert "llm-key" in admin.call("GET", "/api/v1/secrets").text

    for value in (secrets.token_urlsafe(24), original):
        bob_pod = scenarios.hermes_pod(platform.identifier, bob_space)
        admin.call("PUT", "/api/v1/secrets/llm-key", {"kind": "env", "value": value})
        restarted(platform, bob, bob_space, bob_pod)
        assert delivered(platform, bob_space) == value, "the current version"
        assert delivered(platform, carol_space) == "ABSENT"
        assert scenarios.hermes_pod(platform.identifier, carol_space) == carol_pod, (
            "an instance that does not reference the secret was restarted"
        )
        shown = (
            admin.call("GET", "/api/v1/secrets").text
            + admin.call("GET", "/api/v1/audit-events?limit=100").text
        )
        assert value not in shown and original not in shown, (
            "a secret value was returned by the API or written to the audit log"
        )


def set_sealing_key(platform, encoded, wait=True):
    """Replaces the controller's sealing key (base64, as a Secret holds it) and
    restarts the controller; the key is never printed. With `wait`, returns
    when the new controller is rolled out."""
    scenarios.kubectl(
        platform.identifier,
        "patch",
        "secret",
        "hermes-manager-controller",
        "--namespace",
        scenarios.NAMESPACE,
        "--type",
        "merge",
        "--patch",
        json.dumps({"data": {"secret-sealing.key": encoded}}),
        quiet=True,
    )
    scenarios.kubectl(
        platform.identifier,
        "rollout",
        "restart",
        "deployment/hermes-manager-controller",
        "--namespace",
        scenarios.NAMESPACE,
    )
    if wait:
        scenarios.kubectl(
            platform.identifier,
            "rollout",
            "status",
            "deployment/hermes-manager-controller",
            "--namespace",
            scenarios.NAMESPACE,
            "--timeout=240s",
            timeout=260,
        )


def controller_logs(platform):
    """What the controller's Pods have logged, current and last run."""
    logs = []
    for flag in ([], ["--previous"]):
        logs.append(
            scenarios.kubectl(
                platform.identifier,
                "logs",
                "--namespace",
                scenarios.NAMESPACE,
                "--selector",
                "app.kubernetes.io/name=hermes-manager-controller",
                "--all-containers",
                "--tail=-1",
                *flag,
                check=False,
                quiet=True,
            )
        )
    return "\n".join(logs)


def test_a_controller_with_a_wrong_sealing_key_refuses_to_start_and_poisons_nothing(
    destructive_platform,
):
    """The controller opens sealed material and secrets with its private key.
    Started with a different valid key it must stop, not carry on: carrying on
    would seal material for new instances that the right key could never open.
    Existing instances and their credentials are left as they are."""
    platform = destructive_platform
    admin = platform.admin
    bob = platform.assignments["bob"]["instanceId"]
    carol = platform.assignments["carol"]["instanceId"]
    spaces = {
        name: admin.phase(i)["namespace"]
        for name, i in (("bob", bob), ("carol", carol))
    }

    def credentials():
        return {
            name: scenarios.secret_digests(
                platform.identifier, space, "hermes-credentials"
            )
            for name, space in spaces.items()
        }

    held = json.loads(
        scenarios.kubectl(
            platform.identifier,
            "get",
            "secret",
            "hermes-manager-controller",
            "--namespace",
            scenarios.NAMESPACE,
            "--output",
            "json",
            quiet=True,
        )
    )["data"]["secret-sealing.key"]
    before = credentials()
    pods = {
        name: scenarios.hermes_pod(platform.identifier, space)
        for name, space in spaces.items()
    }
    assert delivered(platform, spaces["bob"]) == platform.passwords["llm"]
    assert "sealing_key_mismatch" not in controller_logs(platform), "a control"

    wrong = base64.b64encode(
        X25519PrivateKey.generate().private_bytes(
            Encoding.Raw, PrivateFormat.Raw, NoEncryption()
        )
    ).decode()
    assert wrong != held
    set_sealing_key(platform, wrong, wait=False)
    scenarios.wait(
        lambda: "sealing_key_mismatch" in controller_logs(platform),
        180,
        "the controller with the wrong key never refused to start",
        interval=3,
    )

    # While that controller cannot run, people are still added and the
    # secret still rotates; whichever controller acts must use the right key.
    username = f"late-{secrets.token_hex(3)}"
    directory = scenarios.Directory(platform.ca, platform.passwords["directory"])
    subject = directory.create(username, f"{username}@e2e.test", secrets.token_hex(12))
    late = admin.call(
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
    new = secrets.token_urlsafe(24)
    admin.call("PUT", "/api/v1/secrets/llm-key", {"kind": "env", "value": new})

    set_sealing_key(platform, held)
    scenarios.wait(
        lambda: admin.phase(late["instanceId"])["phase"] == "ready",
        scenarios.READY_TIMEOUT,
        lambda: admin.phase(late["instanceId"]),
        interval=5,
    )
    scenarios.wait(
        lambda: delivered(platform, spaces["bob"]) == new,
        600,
        "the rotated value never reached the instance that references it",
        interval=5,
    )
    assert credentials() == before, "credentials were regenerated"
    assert scenarios.hermes_pod(platform.identifier, spaces["carol"]) == pods["carol"]
    # The instance created in the window is as manageable as any other: the
    # right key opens its material (a poisoned one would be refused here).
    time.sleep(30)
    refused = [
        line for line in controller_logs(platform).splitlines() if "vault" in line
    ]
    assert not refused, "the controller cannot open some instance's material"

    # The platform is shared: the original value back, delivered again, for the
    # cases that follow.
    admin.call(
        "PUT",
        "/api/v1/secrets/llm-key",
        {"kind": "env", "value": platform.passwords["llm"]},
    )
    scenarios.wait(
        lambda: delivered(platform, spaces["bob"]) == platform.passwords["llm"],
        600,
        "the original value never reached the instance that references it",
        interval=5,
    )
