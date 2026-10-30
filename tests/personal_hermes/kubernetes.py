"""Run resource ownership and bootstrap tests on a disposable Kubernetes server.

Uses a separate digest-pinned k3s Docker container and a temporary kubeconfig.
No existing Kubernetes context or deployment credentials are read or modified.
"""

import argparse
import json
import os
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import yaml

from tests.support.ownership import (
    FIXTURE,
    KUBERNETES,
    POSTGRES,
    RUN,
    Cleanup,
    CommandFailed,
    ForeignResource,
    new_run,
    remove_fixture,
    require_fixture,
)

ROOT = Path(__file__).resolve().parents[2]
IMAGE = (
    "rancher/k3s@sha256:"
    "ca7f37d993d82ef0dcdcfecb2e0e2618ea541dbaffc620c8cedebe01a82acd0d"
)


def command(args, timeout=30):
    result = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    assert result.returncode == 0, "Disposable Kubernetes fixture command failed"
    return result.stdout


WORKER_TAG = "hermes-next-worker:test"


def import_image(identifier, tag, temporary, reuse=False):
    """Imports a local Docker image into k3s; returns its digest reference.

    With `reuse`, an image the cluster already has under `tag` is kept, for
    repeated development runs on one cluster.
    """
    reference = tag if "/" in tag.split(":")[0] else "docker.io/library/" + tag
    if reuse:
        listing = command(["docker", "exec", identifier, "ctr", "images", "list"])
        for line in listing.splitlines():
            if line.split()[0] == reference:
                return reference.rsplit(":", 1)[0] + "@" + line.split()[2]
    archive = Path(temporary) / (tag.replace("/", "_").replace(":", "_") + ".tar")
    command(["docker", "save", "--output", str(archive), tag], timeout=600)
    name = "/tmp/" + archive.name
    command(["docker", "cp", str(archive), f"{identifier}:{name}"], timeout=600)
    archive.unlink()
    command(
        ["docker", "exec", identifier, "ctr", "images", "import", name], timeout=900
    )
    command(["docker", "exec", identifier, "rm", name])
    listing = command(["docker", "exec", identifier, "ctr", "images", "list"])
    digest = next(
        line.split()[2] for line in listing.splitlines() if line.split()[0] == reference
    )
    assert digest.startswith("sha256:")
    pinned = reference.rsplit(":", 1)[0] + "@" + digest
    command(["docker", "exec", identifier, "ctr", "images", "tag", reference, pinned])
    return pinned


def import_worker_image(identifier, temporary):
    """Builds runtimes/agent-runtime, imports it and returns a digest reference."""
    command(
        [
            "docker",
            "build",
            "--quiet",
            "--tag",
            WORKER_TAG,
            str(ROOT / "runtimes/agent-runtime"),
        ],
        timeout=600,
    )
    return import_image(identifier, WORKER_TAG, temporary)


def start_k3s(network, name, run):
    """Starts a digest-pinned k3s server container labelled with its run;
    returns its ID."""
    return command(
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
            f"{FIXTURE}={KUBERNETES}",
            "--label",
            f"{RUN}={run}",
            "--volume",
            f"{ROOT / 'tests/personal_hermes/k3s-registries.yaml'}"
            ":/etc/rancher/k3s/registries.yaml:ro",
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


def wait_ready(identifier):
    """Waits for the API server; returns a kubeconfig reaching it from here.

    The kubeconfig holds fixture credentials: keep it in memory or private
    temporary files only.
    """
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
    config["clusters"][0]["cluster"]["server"] = f"https://127.0.0.1:{port['HostPort']}"
    config["contexts"][0]["name"] = "personal-hermes-manager-tests"
    config["current-context"] = "personal-hermes-manager-tests"
    return config


def remove_container(identifier, run=None):
    """Removes exactly this fixture container, after proving it is one."""
    # Docker can report a transient removal conflict while the server
    # finishes exiting, or lose the CLI's response after removing it. Retry
    # only this fixture's exact ID and observe its absence; a daemon failure
    # is never treated as proof of absence.
    for attempt in range(3):
        try:
            remove_fixture(identifier, KUBERNETES, run)
            return
        except ForeignResource:
            raise
        except CommandFailed, subprocess.TimeoutExpired:
            pass
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
            return
        assert attempt < 2, "Disposable Kubernetes fixture cleanup failed"
        time.sleep(0.2)


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
    # Injection (pausing the database) and database creation happen in this
    # container: prove it is a fixture before anything else.
    require_fixture(args.postgres_container, POSTGRES)
    postgres = json.loads(command(["docker", "inspect", args.postgres_container]))[0]
    networks = postgres["NetworkSettings"]["Networks"]
    assert networks, "Disposable PostgreSQL fixture has no network"
    network = "bridge" if "bridge" in networks else next(iter(networks))
    run = new_run()
    name = "personal-hermes-kube-test-" + run
    if args.k3d:
        name = "hermes-native-" + run[:8]
    with Cleanup() as cleanup:
        if args.k3d:
            with socket.socket() as port_socket:
                port_socket.bind(("127.0.0.1", 0))
                api_port = port_socket.getsockname()[1]
            cleanup.add(
                "k3d cluster",
                lambda: command(["k3d", "cluster", "delete", name], timeout=90),
            )
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
            cleanup.add(
                "database network membership",
                lambda: command(
                    [
                        "docker",
                        "network",
                        "disconnect",
                        f"k3d-{name}",
                        args.postgres_container,
                    ]
                ),
            )
            command(
                ["docker", "network", "connect", f"k3d-{name}", args.postgres_container]
            )
        else:
            identifier = start_k3s(network, name, run)
            cleanup.add("k3s container", lambda: remove_container(identifier, run))
        config = wait_ready(identifier)
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
            environment["HERMES_TEST_RUN"] = run
            environment["DATABASE_URL"] = (
                f"postgres://hermes_test@127.0.0.1:{postgres_port}/hermes_test"
            )
            # The rewrite's adapter tests run in-process as the controller's
            # real ServiceAccount; no binary is needed. The worker test
            # needs the repository's terminal backend image in the cluster.
            environment["HERMES_TEST_WORKER_IMAGE"] = import_worker_image(
                identifier, temporary
            )
            environment["HERMES_TEST_K3S_CONTAINER"] = identifier
            environment["HERMES_TEST_POSTGRES_CONTAINER"] = args.postgres_container
            result = subprocess.run(
                [
                    sys.executable,
                    "-m",
                    "tests.support.verification",
                    "run",
                    "rust-cluster",
                    "--output",
                    ".local/verification/rust-cluster.json",
                ],
                cwd=ROOT,
                env=environment,
                timeout=600,
            )
            assert result.returncode == 0, "Real Kubernetes adapter test failed"
            result = subprocess.run(
                [
                    sys.executable,
                    "-m",
                    "tests.support.verification",
                    "run",
                    "rust-controller",
                    "--output",
                    ".local/verification/rust-controller.json",
                ],
                cwd=ROOT,
                env=environment,
                timeout=600,
            )
            assert result.returncode == 0, "Controller process test failed"
            print(
                "PASS: server-side apply, admission and RBAC under the real "
                "ServiceAccount, preconditioned delete, leader election, "
                "controller aborts at every fault point"
            )
            if args.native:
                from tests.personal_hermes.native_e2e import run_e2e

                run_e2e(identifier, temporary, environment)


if __name__ == "__main__":
    main()
