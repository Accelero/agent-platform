"""Run the Rust ownership tests against a fresh disposable Kubernetes API server.

Uses a separate digest-pinned k3s Docker container and a temporary kubeconfig.
No existing Kubernetes context or deployment credentials are read or modified.
"""

import argparse
import json
import os
import subprocess
import tempfile
import time
import uuid
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
IMAGE = (
    "rancher/k3s@sha256:"
    "ca7f37d993d82ef0dcdcfecb2e0e2618ea541dbaffc620c8cedebe01a82acd0d"
)


def command(args, timeout=30):
    result = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    assert result.returncode == 0, "Disposable Kubernetes fixture command failed"
    return result.stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--postgres-container", default="personal-hermes-manager-tests")
    args = parser.parse_args()
    name = "personal-hermes-kube-test-" + uuid.uuid4().hex[:12]
    identifier = None
    try:
        identifier = command(
            [
                "docker",
                "run",
                "--detach",
                "--name",
                name,
                "--privileged",
                "--tmpfs",
                "/run",
                "--tmpfs",
                "/var/run",
                "--label",
                "agent-platform.io/fixture=personal-hermes-kubernetes",
                "--publish",
                "127.0.0.1::6443",
                IMAGE,
                "server",
                "--disable",
                "traefik,servicelb,metrics-server",
                "--tls-san",
                "127.0.0.1",
            ],
            timeout=120,
        ).strip()
        state = json.loads(command(["docker", "inspect", identifier]))[0]
        expected_image = command(
            ["docker", "image", "inspect", IMAGE, "--format", "{{.Id}}"]
        ).strip()
        assert state["Image"] == expected_image
        port = state["NetworkSettings"]["Ports"]["6443/tcp"][0]
        assert port["HostIp"] == "127.0.0.1"
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            try:
                probe = subprocess.run(
                    [
                        "docker",
                        "exec",
                        identifier,
                        "/bin/kubectl",
                        "--kubeconfig",
                        "/etc/rancher/k3s/k3s.yaml",
                        "--request-timeout=3s",
                        "get",
                        "--raw",
                        "/readyz",
                    ],
                    capture_output=True,
                    timeout=5,
                )
            except subprocess.TimeoutExpired:
                probe = None
            if (
                probe is not None
                and probe.returncode == 0
                and probe.stdout.strip() == b"ok"
            ):
                break
            state = json.loads(command(["docker", "inspect", identifier]))[0]
            assert state["State"]["Running"], "Disposable Kubernetes server stopped"
            time.sleep(0.5)
        else:
            raise AssertionError("Disposable Kubernetes server did not become ready")
        # This output includes fixture credentials. Keep it in memory/files only.
        config = yaml.safe_load(
            command(["docker", "exec", identifier, "cat", "/etc/rancher/k3s/k3s.yaml"])
        )
        config["clusters"][0]["cluster"]["server"] = (
            f"https://127.0.0.1:{port['HostPort']}"
        )
        config["contexts"][0]["name"] = "personal-hermes-manager-tests"
        config["current-context"] = "personal-hermes-manager-tests"
        postgres_port = (
            command(["docker", "port", args.postgres_container, "5432/tcp"])
            .strip()
            .splitlines()[0]
            .rsplit(":", 1)[1]
        )
        with tempfile.TemporaryDirectory(
            prefix="personal-hermes-kubernetes-"
        ) as temporary:
            path = Path(temporary) / "kubeconfig"
            path.write_text(yaml.safe_dump(config))
            path.chmod(0o600)
            environment = dict(os.environ)
            environment["HERMES_TEST_KUBECONFIG"] = str(path)
            environment["DATABASE_URL"] = (
                f"postgres://hermes_test@127.0.0.1:{postgres_port}/hermes_test"
            )
            result = subprocess.run(
                [
                    "cargo",
                    "test",
                    "--test",
                    "kubernetes",
                    "--locked",
                    "--",
                    "--ignored",
                ],
                cwd=ROOT / "services/hermes-manager",
                env=environment,
                timeout=600,
            )
            assert result.returncode == 0, "Real Kubernetes ownership test failed"
        print(
            "PASS: isolated Kubernetes creation recovery, namespace/UID ownership "
            "and preconditioned retirement"
        )
    finally:
        if identifier:
            command(["docker", "rm", "--force", "--volumes", identifier])


if __name__ == "__main__":
    main()
