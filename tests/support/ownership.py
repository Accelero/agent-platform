"""Ownership checks and cleanup for disposable test infrastructure.

A container is a fixture only when its labels say so: a name, an image or a
failed inspection proves nothing. Destructive steps (reset, reuse, injection,
removal) run only against resources a run recorded or labelled itself, never
against a name pattern. Finalizers all run, and cleanup failures never replace
the error that ended the run.
"""

from __future__ import annotations

import json
import re
import subprocess
import uuid
from collections.abc import Callable

FIXTURE = "agent-platform.io/fixture"
RUN = "agent-platform.io/run"
KUBERNETES = "personal-hermes-kubernetes"
POSTGRES = "personal-hermes-postgres"
REGISTRY_FIXTURE = "personal-hermes-registry"
DATABASE = re.compile(r"hermes_e2e_[0-9a-f]{8}(?:_restored)?")
RUN_ID = re.compile(r"[0-9a-f]{12}")

Docker = Callable[..., str]


class ForeignResource(AssertionError):
    """The resource is not the disposable fixture the step requires."""


class CommandFailed(RuntimeError):
    """Docker could not answer. This is never evidence about ownership."""


def docker(args, timeout=30):
    """Runs `docker <args>` bounded; a failure names the subcommand only."""
    result = subprocess.run(
        ["docker", *args], capture_output=True, text=True, timeout=timeout
    )
    if result.returncode != 0:
        raise CommandFailed(f"docker {args[0]} failed")
    return result.stdout


def new_run():
    return uuid.uuid4().hex[:12]


def database_comment(run):
    assert RUN_ID.fullmatch(run), "malformed run identifier"
    return f"{RUN}={run}"


def labels(identifier, *, run_docker: Docker = docker):
    """The container's labels. Absent labels are an empty mapping; an
    inspection failure raises and is never read as `not a fixture`."""
    output = run_docker(["inspect", "--format", "{{json .Config.Labels}}", identifier])
    return json.loads(output) or {}


def require_fixture(identifier, kind, run=None, *, run_docker: Docker = docker):
    """Proves `identifier` is a fixture of `kind` (and of `run`, when given).

    Returns the run it belongs to. Other labels are never echoed.
    """
    found = labels(identifier, run_docker=run_docker)
    if found.get(FIXTURE) != kind:
        raise ForeignResource(f"{identifier} is not a {kind} fixture")
    if run is not None and found.get(RUN) != run:
        raise ForeignResource(f"{identifier} belongs to another run")
    return found.get(RUN)


def remove_fixture(identifier, kind, run=None, *, run_docker: Docker = docker):
    """Removes exactly this verified fixture container and its volumes."""
    require_fixture(identifier, kind, run, run_docker=run_docker)
    run_docker(["rm", "--force", "--volumes", identifier], timeout=60)


def run_databases(postgres, run, *, run_docker: Docker = docker):
    """The databases this run created, found by the comment it stored.

    A similar name proves nothing: a database belongs to the run only when its
    comment says so.
    """
    require_fixture(postgres, POSTGRES, run_docker=run_docker)
    output = run_docker(
        [
            "exec",
            postgres,
            "psql",
            "-U",
            "hermes_test",
            "-d",
            "hermes_test",
            "-Atc",
            "SELECT d.datname FROM pg_database d "
            "WHERE shobj_description(d.oid, 'pg_database') = "
            f"'{database_comment(run)}'",
        ]
    )
    names = output.split()
    for name in names:
        assert DATABASE.fullmatch(name), "unexpected database name"
    return names


class Cleanup:
    """Finalizers registered right after each successful allocation.

    They run in reverse order and all of them run. On exit after an error, the
    original error propagates and the failures are attached as notes. On a
    clean exit, failures raise together: a leaked resource fails the job.
    """

    def __init__(self):
        self._finalizers: list[tuple[str, Callable[[], object]]] = []
        self.failures: list[tuple[str, BaseException]] = []
        # True while finalizers run after the body raised, so a finalizer can
        # keep a resource for inspection only when the run failed.
        self.failed = False

    def add(self, description, action):
        self._finalizers.append((description, action))

    def run(self):
        while self._finalizers:
            description, action = self._finalizers.pop()
            try:
                action()
            except Exception as error:
                self.failures.append((description, error))

    def __enter__(self):
        return self

    def __exit__(self, kind, error, traceback):
        self.failed = error is not None
        self.run()
        if error is not None:
            for description, failure in self.failures:
                error.add_note(
                    f"cleanup failed: {description}: {type(failure).__name__}"
                )
            return False
        if self.failures:
            raise ExceptionGroup(
                "cleanup failed",
                [failure for _, failure in self.failures],
            )
        return False
