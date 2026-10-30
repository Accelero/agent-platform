"""A volume the assignment marks read-only is read-only inside the instance.

On a platform of its own, one owner gets two static host-path volumes of the
same kind and ownership: one marked `readOnly` by the assignment, one not. The
writable sibling is the positive control, so a refusal cannot be a permissions
accident, and the host directory must stay as it was.
"""

import subprocess

import pytest

from tests.personal_hermes import native_e2e as scenarios

pytestmark = [
    pytest.mark.e2e,
    pytest.mark.cluster,
    pytest.mark.destructive,
    pytest.mark.slow,
]

ROOT = "/srv/hermes-data"


def on_node(platform, script):
    return subprocess.run(
        ["docker", "exec", platform.identifier, "sh", "-c", script],
        capture_output=True,
        text=True,
        timeout=60,
    )


def attempt(platform, namespace, script):
    """Runs `script` in the owner's Hermes; returns (succeeded, output)."""
    result = subprocess.run(
        [
            "docker",
            "exec",
            platform.identifier,
            "kubectl",
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
        ],
        capture_output=True,
        text=True,
        timeout=120,
    )
    return result.returncode == 0, result.stdout + result.stderr


def test_a_read_only_volume_refuses_writes_while_its_writable_sibling_accepts_them(
    destructive_platform,
):
    platform = destructive_platform
    admin = platform.admin
    for name in ("ro-data", "rw-data"):
        made = on_node(
            platform,
            f"mkdir -p {ROOT}/{name} && echo kept > {ROOT}/{name}/hello"
            f" && chown -R 1000:1000 {ROOT}/{name}",
        )
        assert made.returncode == 0

    template = admin.publish(
        "templates",
        "mounts",
        {
            "schemaVersion": 2,
            "image": platform.images["hermes"],
            "resources": {"cpuMillis": 1000, "memoryMib": 2048},
            "home": {"source": "local", "sizeGib": 1},
            "mounts": {
                "workspace": {"mountPath": "/workspace"},
                "reference": {"mountPath": "/mnt/ro"},
                "scratch": {"mountPath": "/mnt/rw"},
            },
            "defaultVolumes": {"workspace": {"source": "local", "sizeGib": 1}},
        },
    )
    receipt = admin.call(
        "POST",
        "/api/v1/assignments",
        {
            "identity": {
                "issuer": scenarios.ISSUER,
                "subject": scenarios.SUBJECTS["erin"],
            },
            "spec": {
                "access": "enabled",
                "template": {"id": "mounts", "version": template},
                "volumes": {
                    "workspace": {"source": "local", "sizeGib": 1},
                    "reference": {
                        "source": "host",
                        "path": "ro-data",
                        "readOnly": True,
                    },
                    "scratch": {"source": "host", "path": "rw-data"},
                },
                "terminal": platform.state.terminal,
            },
        },
    ).json()
    scenarios.wait(
        lambda: admin.phase(receipt["instanceId"])["phase"] == "ready",
        scenarios.READY_TIMEOUT,
        lambda: (
            f"the instance never became ready: {admin.phase(receipt['instanceId'])}"
        ),
        interval=5,
    )
    namespace = admin.phase(receipt["instanceId"])["namespace"]

    # Both are readable; only the unmarked one is writable.
    for path in ("/mnt/ro", "/mnt/rw"):
        ok, output = attempt(platform, namespace, f"cat {path}/hello")
        assert ok and output.strip() == "kept", f"{path} is not readable: {output}"
    assert attempt(platform, namespace, "touch /mnt/rw/created")[0], "the control"
    assert attempt(platform, namespace, "touch /workspace/created")[0]
    refused, output = attempt(platform, namespace, "touch /mnt/ro/created")
    assert not refused and "Read-only file system" in output, output
    refused, output = attempt(platform, namespace, "echo changed >> /mnt/ro/hello")
    assert not refused and "Read-only file system" in output, output
    refused, output = attempt(platform, namespace, "rm /mnt/ro/hello")
    assert not refused and "Read-only file system" in output, output

    # The host directories are what the mounts say.
    assert on_node(platform, f"cat {ROOT}/ro-data/hello").stdout.strip() == "kept"
    assert on_node(platform, f"test -e {ROOT}/ro-data/created").returncode != 0
    assert on_node(platform, f"test -e {ROOT}/rw-data/created").returncode == 0
