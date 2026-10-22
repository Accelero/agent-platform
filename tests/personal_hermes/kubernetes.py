"""Run resource ownership and bootstrap tests on a disposable Kubernetes server.

Uses a separate digest-pinned k3s Docker container and a temporary kubeconfig.
No existing Kubernetes context or deployment credentials are read or modified.
"""

import argparse
import json
import os
import socket
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
    parser.add_argument(
        "--native",
        action="store_true",
        help="Also commission real cached native images inside Kubernetes",
    )
    parser.add_argument(
        "--k3d",
        action="store_true",
        help="Use a disposable k3d cluster without touching a deployment context",
    )
    args = parser.parse_args()
    postgres = json.loads(command(["docker", "inspect", args.postgres_container]))[0]
    networks = postgres["NetworkSettings"]["Networks"]
    assert networks, "Disposable PostgreSQL fixture has no network"
    network = "bridge" if "bridge" in networks else next(iter(networks))
    name = "personal-hermes-kube-test-" + uuid.uuid4().hex[:12]
    if args.k3d:
        name = "hermes-native-" + uuid.uuid4().hex[:8]
    identifier = None
    try:
        if args.k3d:
            with socket.socket() as port_socket:
                port_socket.bind(("127.0.0.1", 0))
                api_port = port_socket.getsockname()[1]
            command(
                [
                    "k3d",
                    "cluster",
                    "create",
                    name,
                    "--image",
                    IMAGE,
                    "--api-port",
                    f"127.0.0.1:{api_port}",
                    "--no-lb",
                    "--kubeconfig-update-default=false",
                    "--kubeconfig-switch-context=false",
                    "--wait",
                    "--timeout",
                    "120s",
                ],
                timeout=180,
            )
            identifier = command(
                ["docker", "inspect", "--format", "{{.Id}}", f"k3d-{name}-server-0"]
            ).strip()
            command(
                ["docker", "network", "connect", f"k3d-{name}", args.postgres_container]
            )
        else:
            identifier = command(
                [
                    "docker",
                    "run",
                    "--detach",
                    "--name",
                    name,
                    "--privileged",
                    "--network",
                    network,
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
            build = subprocess.run(
                ["cargo", "build", "-p", "hermes-manager", "--locked", "--offline"],
                cwd=ROOT / "services/hermes-manager",
                env=environment,
                timeout=300,
            )
            assert build.returncode == 0, "Controller process fixture build failed"
            environment["HERMES_TEST_MANAGER_BINARY"] = str(
                ROOT / "services/hermes-manager/target/debug/hermes-manager"
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
            if args.native:
                from tests.personal_hermes.native_commissioning import run_fixture

                run_fixture(identifier, args.postgres_container, temporary)
        print(
            "PASS: isolated Kubernetes creation recovery, namespace/UID ownership "
            "and preconditioned retirement; ServiceAccount bootstrap/admission"
        )
    finally:
        if args.k3d:
            subprocess.run(
                [
                    "docker",
                    "network",
                    "disconnect",
                    f"k3d-{name}",
                    args.postgres_container,
                ],
                capture_output=True,
                timeout=30,
            )
            subprocess.run(
                ["k3d", "cluster", "delete", name], capture_output=True, timeout=90
            )
        elif identifier:
            # Docker can report a transient removal conflict while the server
            # finishes exiting. Retry only this fixture's exact container ID;
            # an inspect/daemon failure is never treated as proof of absence.
            for attempt in range(3):
                try:
                    removed = subprocess.run(
                        ["docker", "rm", "--force", "--volumes", identifier],
                        capture_output=True,
                        timeout=30,
                    )
                except subprocess.TimeoutExpired:
                    # The daemon may finish after its CLI loses the response.
                    # Observe this exact container before retrying removal.
                    removed = None
                if removed is not None and removed.returncode == 0:
                    break
                remaining = command(
                    [
                        "docker",
                        "ps",
                        "--all",
                        "--no-trunc",
                        "--filter",
                        f"id={identifier}",
                        "--format",
                        "{{.ID}}",
                    ]
                )
                if not remaining.strip():
                    break
                assert attempt < 2, "Disposable Kubernetes fixture cleanup failed"
                time.sleep(0.2)


if __name__ == "__main__":
    main()
