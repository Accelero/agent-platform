"""The installed platform on the run-owned k3d cluster, for independent cases.

This is the bring-up of the standalone end-to-end program, factored for pytest:
a run-owned PostgreSQL on the cluster's network, every image imported into
every node, Keycloak and the fake model, the chart with its operator Jobs, and
the published catalog with two commissioned owners. Cases receive the result
and run alone, in any order; the program's sequential entry point stays until
its assertions are all collected.

Nothing here prints or retains a credential beyond the run's temporary
directory.
"""

from __future__ import annotations

import secrets
import tempfile
import uuid
from contextlib import ExitStack
from pathlib import Path
from types import SimpleNamespace

import requests

from tests.personal_hermes import native_e2e as e2e
from tests.personal_hermes.kubernetes import (
    WORKER_TAG,
    command,
    import_image,
)
from tests.support.cluster import Cluster
from tests.support.ownership import (
    FIXTURE,
    POSTGRES,
    RUN,
    Cleanup,
    remove_fixture,
)

POSTGRES_IMAGE = (
    "postgres@sha256:2a0d0fe14825b0939f78a8cad5cd4e6aa68bf94d0e5dd96e24b6d23af4315545"
)


class PlatformUnavailable(RuntimeError):
    """A prerequisite of the installed platform is missing."""


def require_local_images():
    """The artifacts under test must exist locally; nothing is built here."""
    for tag in (e2e.MANAGER_IMAGE, e2e.HERMES_IMAGE):
        result = command_status(["docker", "image", "inspect", tag])
        if result != 0:
            raise PlatformUnavailable(f"the image {tag} is not built")


def command_status(args):
    import subprocess

    return subprocess.run(args, capture_output=True, timeout=60).returncode


def start_postgres(cluster: Cluster, cleanup: Cleanup):
    """A run-owned PostgreSQL on the cluster's network; returns its name and
    the database URL pods use."""
    name = f"hermes-pg-{cluster.run}"
    network = f"k3d-{cluster.name}"
    cleanup.add(name, lambda: remove_fixture(name, POSTGRES, cluster.run))
    command(
        [
            "docker",
            "run",
            "--detach",
            "--name",
            name,
            "--label",
            f"{FIXTURE}={POSTGRES}",
            "--label",
            f"{RUN}={cluster.run}",
            "--network",
            network,
            "--env",
            "POSTGRES_USER=hermes_test",
            "--env",
            "POSTGRES_DB=hermes_test",
            "--env",
            "POSTGRES_HOST_AUTH_METHOD=trust",
            POSTGRES_IMAGE,
        ],
        timeout=300,
    )

    def ready():
        probe = command_status(
            [
                "docker",
                "exec",
                name,
                "pg_isready",
                # The image's temporary init server answers on the unix
                # socket only; the final server is the one on TCP.
                "-h",
                "127.0.0.1",
                "-U",
                "hermes_test",
                "-d",
                "hermes_test",
            ]
        )
        return probe == 0

    cluster.wait_for(ready, 90, "PostgreSQL", interval=1)
    database = "hermes_e2e_" + uuid.uuid4().hex[:8]
    command(
        [
            "docker",
            "exec",
            name,
            "psql",
            "-U",
            "hermes_test",
            "-d",
            "hermes_test",
            "-c",
            f"CREATE DATABASE {database}",
        ]
    )
    address = e2e.container_ip(name, network)
    return name, database, f"postgres://hermes_test@{address}:5432/{database}"


def bring_up(cluster: Cluster, directory: Path, cleanup: Cleanup):
    """The installed platform with Bob's and Carol's commissioned instances."""
    require_local_images()
    server = cluster.servers[0]
    network = f"k3d-{cluster.name}"
    postgres, database, database_url = start_postgres(cluster, cleanup)

    command(["docker", "pull", "--quiet", e2e.KEYCLOAK_IMAGE], timeout=900)
    command(["docker", "tag", e2e.KEYCLOAK_IMAGE, "hermes-e2e-keycloak:pinned"])
    command(
        [
            "docker",
            "build",
            "--quiet",
            "--tag",
            WORKER_TAG,
            str(e2e.ROOT / "runtimes/agent-runtime"),
        ],
        timeout=600,
    )
    references = {}
    with tempfile.TemporaryDirectory(prefix="platform-images-") as scratch:
        for key, tag in (
            ("manager", e2e.MANAGER_IMAGE),
            ("hermes", e2e.HERMES_IMAGE),
            ("worker", WORKER_TAG),
            ("keycloak", "hermes-e2e-keycloak:pinned"),
        ):
            pinned = {import_image(node, tag, scratch) for node in cluster.nodes}
            if len(pinned) != 1:
                raise PlatformUnavailable(f"{tag} differs between nodes")
            references[key] = pinned.pop()
    images = {name: references[name] for name in ("manager", "hermes", "worker")}

    node_ip = e2e.container_ip(server, network)
    command(
        [
            "docker",
            "exec",
            server,
            "sh",
            "-c",
            "mkdir -p /srv/hermes-data/carol && chown 1000:1000 /srv/hermes-data/carol",
        ]
    )
    tls_directory = directory / "tls"
    tls_directory.mkdir()
    tls = e2e.certificates(tls_directory)
    ca = str(tls_directory / "ca.crt")
    passwords = {name: secrets.token_urlsafe(18) for name in e2e.SUBJECTS}
    for name in ("directory", "llm", "registry"):
        passwords[name] = secrets.token_urlsafe(24)

    e2e.keycloak(server, references["keycloak"], tls, passwords)
    e2e.fake_model(server, images["hermes"])
    for namespace, name in (("keycloak", "keycloak"), ("model", "fake-model")):
        e2e.kubectl(
            server,
            "rollout",
            "status",
            "--namespace",
            namespace,
            f"deployment/{name}",
            "--timeout=300s",
            timeout=320,
        )
    stack = ExitStack()
    cleanup.add("name routing", stack.close)
    stack.enter_context(e2e.routed(node_ip))
    e2e.wait(
        lambda: (
            requests.get(
                f"{e2e.ISSUER}/.well-known/openid-configuration",
                verify=ca,
                timeout=5,
            ).status_code
            == 200
        ),
        300,
        "Keycloak did not serve the realm",
    )
    owned = e2e.installation()
    e2e.install(
        server,
        cluster.kubeconfig,
        str(directory),
        images,
        database_url,
        tls,
        server,
        owned,
        e2e.FRESH,
    )
    state = e2e.commission(server, images, ca, passwords)
    return SimpleNamespace(
        cluster=cluster,
        identifier=server,
        postgres=postgres,
        ca=ca,
        passwords=passwords,
        images=images,
        admin=state.admin,
        assignments=state.assignments,
        state=state,
        # What a restore onto a second cluster needs.
        network=network,
        database=database,
        directory=directory,
        tls=tls,
        owned=owned,
    )
