"""Verify native bootstrap against disposable PostgreSQL and the actual backend."""

from __future__ import annotations

import argparse
import json
import time
import urllib.request
import uuid

from agent_platform.command import run

POSTGRES = (
    "postgres:17.9-bookworm@sha256:"
    "47f917f7409eacd22fc5dfb1dee634e1b55cf0c01d1a7eb701be2227a03e0641"
)


def check(
    image: str,
    *,
    binary: str = "/usr/local/bin/multica-bootstrap",
    server: str = "",
) -> None:
    suffix = uuid.uuid4().hex[:12]
    network, database, backend = (
        "bootstrap-" + suffix + tail for tail in ("", "-db", "-api")
    )
    url = f"postgres://postgres:bootstrap-fixture@{database}:5432/postgres?sslmode=disable"
    installation = "bootstrap-fixture"

    def native(action: str, **values: object) -> dict:
        payload = {"action": action, "installation": installation, **values}
        output = run(
            [
                "docker",
                "run",
                "--rm",
                "--interactive",
                "--network",
                network,
                "--env",
                "DATABASE_URL=" + url,
                "--entrypoint",
                binary,
                image,
            ],
            data=json.dumps(payload).encode(),
            timeout=45,
        )
        return json.loads(output)

    def sql(query: str) -> str:
        return (
            run(
                [
                    "docker",
                    "exec",
                    database,
                    "psql",
                    "--username",
                    "postgres",
                    "--tuples-only",
                    "--no-align",
                    "--set",
                    "ON_ERROR_STOP=1",
                    "--command",
                    query,
                ],
                timeout=30,
            )
            .decode()
            .strip()
        )

    run(["docker", "network", "create", network])
    try:
        run(
            [
                "docker",
                "run",
                "--detach",
                "--name",
                database,
                "--network",
                network,
                "--env",
                "POSTGRES_PASSWORD=bootstrap-fixture",
                POSTGRES,
            ],
            timeout=180,
        )
        deadline = time.monotonic() + 120
        while True:
            try:
                run(
                    ["docker", "exec", database, "pg_isready", "-U", "postgres"],
                    timeout=10,
                )
                break
            except RuntimeError:
                if time.monotonic() >= deadline:
                    raise
                time.sleep(1)
        run(
            [
                "docker",
                "run",
                "--detach",
                "--name",
                backend,
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
                *(["--entrypoint", server] if server else []),
                image,
            ]
        )
        deadline = time.monotonic() + 180
        while True:
            try:
                initial = native("inspect")
                break
            except RuntimeError:
                if time.monotonic() >= deadline:
                    raise
                time.sleep(1)
        assert initial["state"] == "empty"
        tokens = native("generate")
        request = {
            "email": "owner@example.org",
            "name": "Research",
            "slug": "research",
            "prefix": "RES",
            **tokens,
        }
        first = native("ensure", **request)
        assert native("ensure", **request) == first
        assert sql('SELECT count(*) FROM "user"') == "1"
        assert sql("SELECT count(*) FROM workspace") == "1"
        assert sql("SELECT count(*) FROM issue_status WHERE is_system") == "7"
        assert sql("SELECT count(*) FROM member WHERE role='owner'") == "1"
        assert sql("SELECT count(*) FROM personal_access_token") == "2"
        address = run(["docker", "port", backend, "8080/tcp"]).decode().strip()
        api = "http://" + address
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

        def call(path: str, data: dict | None = None) -> dict | list:
            req = urllib.request.Request(
                api + path,
                data=None if data is None else json.dumps(data).encode(),
                headers={
                    "Authorization": "Bearer " + tokens["daemon"],
                    "X-Workspace-ID": first["workspace"],
                    "Content-Type": "application/json",
                },
            )
            with opener.open(req, timeout=15) as response:
                return json.loads(response.read(1048576))

        assert call("/api/me")["email"] == "owner@example.org"
        daemon = str(uuid.uuid4())
        registration = {
            "workspace_id": first["workspace"],
            "daemon_id": daemon,
            "device_name": "Bootstrap fixture",
            "runtimes": [
                {"name": "Hermes fixture", "type": "hermes", "status": "online"}
            ],
        }
        registered = call("/api/daemon/register", registration)
        runtimes = (
            registered if isinstance(registered, list) else registered["runtimes"]
        )
        runtime = runtimes[0]["id"]
        agent = call(
            "/api/agents",
            {
                "name": "Assistant fixture",
                "runtime_id": runtime,
                "permission_mode": "private",
                "max_concurrent_tasks": 1,
                "custom_args": ["-p", "personal-assistant"],
            },
        )
        native("complete", user=first["user"], workspace=first["workspace"])
        assert native("inspect")["state"] == "initialized"
        # Explicit rotation retains native IDs and user edits, even for revoked tokens.
        sql(
            "UPDATE workspace SET name='User edited'; "
            "UPDATE personal_access_token SET revoked=true"
        )
        renewed = native("generate")
        repaired = native(
            "ensure",
            **{
                **request,
                **renewed,
                "force": True,
                "daemon_id": daemon,
                "previous": tokens["daemon"],
                "workspace": first["workspace"],
            },
        )
        assert (
            repaired["user"] == first["user"]
            and repaired["workspace"] == first["workspace"]
        )
        assert sql("SELECT name FROM workspace") == "User edited"
        native(
            "complete",
            user=first["user"],
            workspace=first["workspace"],
            previous=tokens["daemon"],
            previous_profile=tokens["profile"],
            **renewed,
        )
        assert (
            sql("SELECT count(*) FROM personal_access_token WHERE NOT revoked") == "2"
        )
        tokens.update(renewed)
        restored_runtime = call("/api/daemon/register", registration)
        restored_runtime = (
            restored_runtime
            if isinstance(restored_runtime, list)
            else restored_runtime["runtimes"]
        )
        assert restored_runtime[0]["id"] == runtime
        assert call("/api/agents/" + agent["id"])["id"] == agent["id"]
        # A second installation cannot adopt these rows just because an email matches.
        installation = "unrecognized-installation"
        try:
            native("ensure", **{**request, **native("generate")})
        except RuntimeError:
            pass
        else:
            raise AssertionError("Foreign state was adopted")
        assert sql('SELECT count(*) FROM "user"') == "1"
        assert sql("SELECT count(*) FROM workspace") == "1"
        print(
            "Native bootstrap verified: fresh, retry, owner login, "
            "runtime/agent IDs, force repair and foreign-state refusal."
        )
    finally:
        for name in (backend, database):
            try:
                run(["docker", "rm", "--force", "--volumes", name], timeout=30)
            except RuntimeError:
                pass
        run(["docker", "network", "rm", network], timeout=30)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--backend", required=True)
    parser.add_argument("--binary", default="/usr/local/bin/multica-bootstrap")
    parser.add_argument("--server", default="")
    args = parser.parse_args()
    check(args.backend, binary=args.binary, server=args.server)


if __name__ == "__main__":
    main()
