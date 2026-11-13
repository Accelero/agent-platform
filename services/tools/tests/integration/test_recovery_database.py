"""Prove transactional application restore using real PostgreSQL 18 clients."""

import subprocess
import time
import uuid
from pathlib import Path
from unittest.mock import MagicMock, patch

import pytest
from platform_recovery import runtime as state

pytestmark = pytest.mark.image


def run(args, *, data=None, timeout=60, **kwargs):
    try:
        return subprocess.run(
            args, input=data, capture_output=True, check=True, timeout=timeout
        ).stdout
    except subprocess.CalledProcessError as error:
        error.add_note(error.stderr.decode())
        raise


class ContainerPostgres:
    def __init__(self, name):
        self.name = name

    def query(self, sql, database="postgres"):
        return (
            run(
                [
                    "docker",
                    "exec",
                    self.name,
                    "psql",
                    "-h",
                    "127.0.0.1",
                    "-U",
                    "postgres",
                    "-d",
                    database,
                    "-At",
                    "-v",
                    "ON_ERROR_STOP=1",
                    "-c",
                    sql,
                ]
            )
            .decode()
            .strip()
        )


@pytest.fixture
def postgres_sql():
    try:
        run(["docker", "info"], timeout=10)
    except OSError, subprocess.SubprocessError:
        pytest.skip("Docker is required for local PostgreSQL recovery tests")
    name = "platform-recovery-pg-" + uuid.uuid4().hex
    run(
        [
            "docker",
            "run",
            "--rm",
            "-d",
            "--name",
            name,
            "-e",
            "POSTGRES_HOST_AUTH_METHOD=trust",
            "postgres:18.7-trixie@sha256:9f4fd4aa918f52ae3d6f3b687f1de90d8f867cd4d520021545a9a50c120eae3b",
        ],
        timeout=120,
    )
    container = ContainerPostgres(name)
    try:
        for _ in range(60):
            try:
                container.query("SELECT 1")
                break
            except subprocess.SubprocessError:
                time.sleep(0.5)
        else:
            pytest.fail("Disposable PostgreSQL did not start")
        yield container
    finally:
        run(["docker", "rm", "-f", name])


def _as_role(
    container: str,
    role: str,
    database: str,
    args: list[str],
    **kwargs: object,
) -> bytes:
    """Run a PostgreSQL client inside the fixture container as the application role."""
    return run(
        [
            "docker",
            "exec",
            "-i",
            "-e",
            "PGUSER=" + role,
            "-e",
            "PGDATABASE=" + database,
            container,
            *args,
        ],
        **kwargs,
    )


def _inside(container: str, staging: Path, argument: str) -> str:
    """Copy a staged Job file into the container and return its container path."""
    if not argument.startswith(str(staging)):
        return argument
    destination = "/var/lib/postgresql/" + Path(argument).name
    if Path(argument).name != "restore.sql":
        run(["docker", "cp", argument, container + ":" + destination])
    return destination


def _seed_application_database(container: str, role: str, database: str) -> None:
    _as_role(
        container,
        role,
        database,
        ["psql", "-X", "--set", "ON_ERROR_STOP=1"],
        data=(
            b"CREATE TABLE agent_task_queue "
            b"(status text, completed_at timestamptz, error text);"
            b"CREATE TABLE issue_wakeup (enabled boolean, next_fire_at timestamptz);"
            b"CREATE TABLE autopilot (status text);"
            b"COMMENT ON TABLE agent_task_queue IS 'native queue';"
            b"INSERT INTO agent_task_queue (status) VALUES ('running'), ('completed');"
            b"INSERT INTO issue_wakeup VALUES (true, now());"
            b"INSERT INTO autopilot VALUES ('active');"
        ),
    )


@pytest.mark.parametrize("sanitize_failure", [False, True])
def test_database_restore_runs_as_application_role(
    sanitize_failure: bool,
    tmp_path: Path,
    postgres_sql: ContainerPostgres,
) -> None:
    """Restore a real dump as the application role onto superuser-owned extensions."""
    role = "multica_fixture_" + uuid.uuid4().hex[:12]
    source, target = role + "_source", role + "_target"
    postgres_sql.query(f"CREATE ROLE {role} LOGIN")
    for database in (source, target):
        postgres_sql.query(f"CREATE DATABASE {database} OWNER {role}")
        # Production databases get their extensions from the CNPG superuser.
        postgres_sql.query(
            "CREATE EXTENSION pgcrypto; CREATE EXTENSION pg_trgm;",
            database=database,
        )
    _seed_application_database(postgres_sql.name, role, source)
    staging = tmp_path / "staging"
    staging.mkdir()
    datasets = tmp_path / "datasets"
    (datasets / "multica-uploads").mkdir(parents=True)
    recovery_config = {"recovery": {"fence": "multica-uploads/.altiera-maintenance"}}
    (staging / "database.dump").write_bytes(
        _as_role(postgres_sql.name, role, source, ["pg_dump", "-Fc"]),
    )

    inject_failure = sanitize_failure

    def job_command(args: list[str], **kwargs: object) -> bytes:
        if inject_failure and args[0] == "psql" and "--single-transaction" in args:
            args = [*args[:-1], args[-1] + "SELECT missing_fixture_function();"]
        if "--file" in args and args[0] == "pg_restore":
            output = args.index("--file") + 1
            destination = "/var/lib/postgresql/restore.sql"
            translated = [
                _inside(postgres_sql.name, staging, item)
                if index != output
                else destination
                for index, item in enumerate(args)
            ]
            result = _as_role(postgres_sql.name, role, target, translated, **kwargs)
            Path(args[output]).write_bytes(
                run(["docker", "exec", postgres_sql.name, "cat", destination])
            )
            return result
        translated = [_inside(postgres_sql.name, staging, item) for item in args]
        if "--dbname" in translated:
            translated[translated.index("--dbname") + 1] = target
        return _as_role(postgres_sql.name, role, target, translated, **kwargs)

    cluster = MagicMock()
    cluster.read_record.return_value = {"snapshot": "fixture"}
    with (
        patch.object(state, "STAGING", staging),
        patch.object(state, "DATASETS", datasets),
        patch.object(state, "Cluster", return_value=cluster),
        patch.object(state, "run", side_effect=job_command),
    ):
        if sanitize_failure:
            with pytest.raises(subprocess.CalledProcessError):
                state.restore_database(recovery_config)
            assert (
                postgres_sql.query(
                    "SELECT count(*) FROM pg_tables WHERE schemaname='public'",
                    database=target,
                )
                == "0"
            )
            inject_failure = False
        result = state.restore_database(recovery_config)
    restored = postgres_sql.query(
        "SELECT concat_ws('|', "
        "(SELECT string_agg(status, ',' ORDER BY status) FROM agent_task_queue), "
        "obj_description('agent_task_queue'::regclass), "
        "(SELECT bool_or(enabled) FROM issue_wakeup), "
        "(SELECT status FROM autopilot))",
        database=target,
    )
    if result != {"database_restored": True, "dispatch_paused": True} or (
        restored != "cancelled,completed|native queue|f|paused"
    ):
        pytest.fail("Application-role restore lost data or replayed work.")
