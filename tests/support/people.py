"""New people and instances for cases that must not touch the commissioned ones.

A case makes its own directory person with an assignment, waits for the
instance and works in it. Nothing here prints a credential.
"""

from __future__ import annotations

import secrets

from tests.personal_hermes import native_e2e as scenarios


def template(platform, **extra):
    """A published-template definition shaped like the commissioned `personal`
    one (a workspace volume and mount), with `extra` fields added."""
    return {
        "schemaVersion": 2,
        "image": platform.images["hermes"],
        "resources": {"cpuMillis": 1000, "memoryMib": 2048},
        "home": {"source": "local", "sizeGib": 1},
        "mounts": {"workspace": {"mountPath": "/workspace"}},
        "defaultVolumes": {"workspace": {"source": "local", "sizeGib": 1}},
        **extra,
    }


def person(platform, prefix, spec):
    """A new directory person with the assignment `spec`; returns the receipt."""
    username = f"{prefix}-{secrets.token_hex(3)}"
    directory = scenarios.Directory(platform.ca, platform.passwords["directory"])
    subject = directory.create(username, f"{username}@e2e.test", secrets.token_hex(12))
    return platform.admin.call(
        "POST",
        "/api/v1/assignments",
        {
            "identity": {"issuer": scenarios.ISSUER, "subject": subject},
            "spec": {"access": "enabled", **spec},
        },
    ).json()


def ready(platform, receipt):
    """Waits for the instance to be ready; returns its namespace."""
    scenarios.wait(
        lambda: platform.admin.phase(receipt["instanceId"])["phase"] == "ready",
        scenarios.READY_TIMEOUT,
        lambda: platform.admin.phase(receipt["instanceId"]),
        interval=5,
    )
    return platform.admin.phase(receipt["instanceId"])["namespace"]
