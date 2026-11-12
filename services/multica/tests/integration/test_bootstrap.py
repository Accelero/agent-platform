"""Native bootstrap against a disposable PostgreSQL and the built backend image.

The module starts one backend on an empty database. The `enrolled` fixture
performs the fresh enrollment; each test then checks one property of it. The
release builder reruns these `artifact` cases on the pushed backend image.
"""

from __future__ import annotations

import json
import subprocess
import time
import urllib.request
import uuid
from dataclasses import dataclass, field

import pytest

from tests.support.images import require_docker

pytestmark = [pytest.mark.image, pytest.mark.artifact]
POSTGRES = (
    "postgres:17.9-bookworm@sha256:"
    "47f917f7409eacd22fc5dfb1dee634e1b55cf0c01d1a7eb701be2227a03e0641"
)
BINARY = "/usr/local/bin/multica-bootstrap"
INSTALLATION = "bootstrap-fixture"


def docker(*args: str, timeout: float = 60, data: bytes | None = None, check=True):
    result = subprocess.run(
        ["docker", *args],
        input=data,
        capture_output=True,
        timeout=timeout,
        check=False,
    )
    if check:
        assert result.returncode == 0, result.stderr.decode(errors="replace")[-4000:]
    return result


def eventually(attempt, *, seconds: float):
    deadline = time.monotonic() + seconds
    while True:
        try:
            return attempt()
        except AssertionError, OSError:
            if time.monotonic() >= deadline:
                raise
            time.sleep(1)


@dataclass
class Backend:
    """A running backend and its database, driven like the bootstrap Job."""

    image: str
    network: str
    database: str
    container: str
    url: str
    api: str = ""
    tokens: dict = field(default_factory=dict)

    def native(self, action: str, installation: str = INSTALLATION, **values):
        payload = {"action": action, "installation": installation, **values}
        result = docker(
            "run",
            "--rm",
            "--interactive",
            "--network",
            self.network,
            "--env",
            "DATABASE_URL=" + self.url,
            "--entrypoint",
            BINARY,
            self.image,
            data=json.dumps(payload).encode(),
            timeout=45,
            check=False,
        )
        if result.returncode:
            raise BootstrapRefused(result.stderr.decode(errors="replace")[-2000:])
        return json.loads(result.stdout)

    def sql(self, query: str) -> str:
        return (
            docker(
                "exec",
                self.database,
                "psql",
                "--username",
                "postgres",
                "--tuples-only",
                "--no-align",
                "--set",
                "ON_ERROR_STOP=1",
                "--command",
                query,
                timeout=30,
            )
            .stdout.decode()
            .strip()
        )

    def call(self, path: str, token: str, workspace: str, data: dict | None = None):
        request = urllib.request.Request(
            self.api + path,
            data=None if data is None else json.dumps(data).encode(),
            headers={
                "Authorization": "Bearer " + token,
                "X-Workspace-ID": workspace,
                "Content-Type": "application/json",
            },
        )
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with opener.open(request, timeout=15) as response:
            return json.loads(response.read(1048576))


class BootstrapRefused(RuntimeError):
    """The bootstrap binary exited with an error."""


@pytest.fixture(scope="module")
def network():
    require_docker()
    name = "bootstrap-" + uuid.uuid4().hex[:12]
    docker("network", "create", name)
    yield name
    docker("network", "rm", name, timeout=30, check=False)


@pytest.fixture(scope="module")
def database(network):
    name = network + "-db"
    docker(
        "run",
        "--detach",
        "--name",
        name,
        "--network",
        network,
        "--env",
        "POSTGRES_PASSWORD=bootstrap-fixture",
        POSTGRES,
        timeout=300,
    )
    try:
        eventually(
            lambda: docker("exec", name, "pg_isready", "-U", "postgres", timeout=10),
            seconds=120,
        )
        yield name
    finally:
        docker("rm", "--force", "--volumes", name, timeout=30, check=False)


@pytest.fixture(scope="module")
def backend(platform_image, network, database):
    image = platform_image("multica-backend")
    url = (
        "postgres://postgres:bootstrap-fixture@"
        f"{database}:5432/postgres?sslmode=disable"
    )
    name = network + "-api"
    docker(
        "run",
        "--detach",
        "--name",
        name,
        "--network",
        network,
        "--publish",
        "127.0.0.1::8080",
        "--env",
        "DATABASE_URL=" + url,
        "--env",
        "JWT_SECRET=" + "bootstrap-fixture-" * 4,
        "--env",
        "SELF_HOST_TELEMETRY_ENABLED=false",
        image,
        timeout=300,
    )
    try:
        current = Backend(image, network, database, name, url)
        address = docker("port", name, "8080/tcp").stdout.decode().split()[0]
        current.api = "http://" + address

        def migrated():
            try:
                return current.native("inspect")
            except BootstrapRefused as error:
                raise AssertionError(str(error)) from None

        # The backend applies its migrations at startup; wait until they exist.
        assert eventually(migrated, seconds=180)["state"] == "empty"
        yield current
    finally:
        docker("rm", "--force", "--volumes", name, timeout=30, check=False)


@pytest.fixture(scope="module")
def enrolled(backend):
    """Run the fresh enrollment once, as the first bootstrap Job does."""
    tokens = backend.native("generate")
    request = {
        "email": "owner@example.org",
        "name": "Research",
        "slug": "research",
        "prefix": "RES",
        **tokens,
    }
    first = backend.native("ensure", **request)
    return {"request": request, "tokens": tokens, "first": first}


def test_fresh_enrollment_is_idempotent_and_the_owner_can_log_in(backend, enrolled):
    assert backend.native("ensure", **enrolled["request"]) == enrolled["first"]
    assert backend.sql('SELECT count(*) FROM "user"') == "1"
    assert backend.sql("SELECT count(*) FROM workspace") == "1"
    assert backend.sql("SELECT count(*) FROM issue_status WHERE is_system") == "7"
    assert backend.sql("SELECT count(*) FROM member WHERE role='owner'") == "1"
    assert backend.sql("SELECT count(*) FROM personal_access_token") == "2"
    me = backend.call(
        "/api/me", enrolled["tokens"]["daemon"], enrolled["first"]["workspace"]
    )
    assert me["email"] == "owner@example.org"


def test_another_installation_cannot_adopt_existing_state(backend, enrolled):
    """Rows are never adopted just because an email matches."""
    with pytest.raises(BootstrapRefused):
        backend.native(
            "ensure",
            installation="unrecognized-installation",
            **{**enrolled["request"], **backend.native("generate")},
        )
    assert backend.sql('SELECT count(*) FROM "user"') == "1"
    assert backend.sql("SELECT count(*) FROM workspace") == "1"


def test_forced_repair_keeps_native_ids_and_user_edits(backend, enrolled):
    """Explicit rotation retains IDs and edits, even for revoked tokens."""
    first, tokens = enrolled["first"], dict(enrolled["tokens"])
    workspace = first["workspace"]
    daemon = str(uuid.uuid4())
    registration = {
        "workspace_id": workspace,
        "daemon_id": daemon,
        "device_name": "Bootstrap fixture",
        "runtimes": [{"name": "Hermes fixture", "type": "hermes", "status": "online"}],
    }

    def runtimes(token):
        registered = backend.call(
            "/api/daemon/register", token, workspace, registration
        )
        return registered if isinstance(registered, list) else registered["runtimes"]

    runtime = runtimes(tokens["daemon"])[0]["id"]
    agent = backend.call(
        "/api/agents",
        tokens["daemon"],
        workspace,
        {
            "name": "Assistant fixture",
            "runtime_id": runtime,
            "permission_mode": "private",
            "max_concurrent_tasks": 1,
            "custom_args": [],
        },
    )
    backend.native("complete", user=first["user"], workspace=workspace)
    assert backend.native("inspect")["state"] == "initialized"
    backend.sql(
        "UPDATE workspace SET name='User edited'; "
        "UPDATE personal_access_token SET revoked=true"
    )
    renewed = backend.native("generate")
    repaired = backend.native(
        "ensure",
        **{
            **enrolled["request"],
            **renewed,
            "force": True,
            "daemon_id": daemon,
            "previous": tokens["daemon"],
            "workspace": workspace,
        },
    )
    assert (repaired["user"], repaired["workspace"]) == (first["user"], workspace)
    assert backend.sql("SELECT name FROM workspace") == "User edited"
    backend.native(
        "complete",
        user=first["user"],
        workspace=workspace,
        previous=tokens["daemon"],
        previous_profile=tokens["profile"],
        **renewed,
    )
    active = "SELECT count(*) FROM personal_access_token WHERE NOT revoked"
    assert backend.sql(active) == "2"
    assert runtimes(renewed["daemon"])[0]["id"] == runtime
    found = backend.call("/api/agents/" + agent["id"], renewed["daemon"], workspace)
    assert found["id"] == agent["id"]
