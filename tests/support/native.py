"""The installed Hermes image started the way a Pod starts it: non-root, a
read-only root filesystem, no network and only the scoped mounts.

The image under test is named by `HERMES_TEST_NATIVE_IMAGE`; there is no
default and nothing is built or downloaded here. Fixture credentials live in a
temporary directory and are never printed.
"""

from __future__ import annotations

import json
import os
import secrets
import subprocess
import time
import uuid
from pathlib import Path

from tests.support.ownership import RUN, Cleanup, new_run, remove_fixture

NATIVE_FIXTURE = "personal-hermes-native"
ISSUER = "https://sso.example.org/realms/agents"
PYTHON = "/opt/hermes/.venv/bin/python"
MOUNTS = [
    ("home", "/opt/data"),
    ("workspace", "/workspace"),
    ("owner", "/run/hermes/owner"),
    ("trust", "/run/hermes/execution-ca"),
    ("credentials", "/run/hermes/credentials"),
]
READ_ONLY = {"owner", "credentials", "trust"}


def image():
    """The image under test; a missing name is a configuration error."""
    name = os.environ.get("HERMES_TEST_NATIVE_IMAGE")
    if not name:
        raise RuntimeError("HERMES_TEST_NATIVE_IMAGE names the image under test")
    return name


class CommandFailed(RuntimeError):
    """A container command failed. Its output is private and not retained."""


def docker(args, *, timeout=60, data=None):
    result = subprocess.run(
        ["docker", *args], capture_output=True, input=data, timeout=timeout
    )
    if result.returncode != 0:
        raise CommandFailed(f"docker {args[0]} failed")
    return result.stdout


def openssl_ca(directory: Path, target: Path):
    """A throwaway CA; its key stays outside every mounted directory."""
    subprocess.run(
        [
            "openssl",
            "req",
            "-x509",
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:P-256",
            "-nodes",
            "-keyout",
            str(directory / "ca.key"),
            "-out",
            str(target),
            "-days",
            "1",
            "-subj",
            "/CN=personal-hermes-test-ca",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
        ],
        capture_output=True,
        check=True,
        timeout=30,
    )


class Layout:
    """The mounted directories of one instance, owned by the data identity."""

    def __init__(self, directory: Path):
        self.root = directory
        directory.chmod(0o755)
        for part in ["home", "workspace", "owner", "init", "credentials", "trust"]:
            (directory / part).mkdir(mode=0o755)
            os.chown(directory / part, 1000, 1000)
        self.context = {
            "schemaVersion": 1,
            "installationId": str(uuid.uuid4()),
            "instanceId": str(uuid.uuid4()),
            "materialId": str(uuid.uuid4()),
            "owner": {"issuer": ISSUER, "subject": "owner-a"},
            "template": {"id": "personal-default", "version": 1},
        }
        (directory / "owner/context.json").write_text(json.dumps(self.context))
        (directory / "init/init-key").write_text(secrets.token_hex(32))
        self.token = secrets.token_hex(32)
        (directory / "credentials/dashboard-session-token").write_text(self.token)
        with_ca = directory.parent / f"{directory.name}-ca"
        with_ca.mkdir()
        openssl_ca(with_ca, directory / "trust/oidc-ca.crt")
        # No model key or login, shared service credentials or prompts.
        self.settings = {
            "manager_url": "https://manager.example.org/execution",
            "token_file": "/run/hermes/credentials/execution-client-token",
            "ca_file": "/run/hermes/execution-ca/ca.crt",
            "mode": "per_session",
            "ssh_key": "/run/hermes/credentials/id_ed25519",
            "acquire_timeout_seconds": 180,
        }
        self.config = {
            "terminal": {"backend": "managed_execution", "cwd": "/workspace"},
            "plugins": {
                "enabled": ["managed_execution"],
                "disabled": [],
                "entries": {"managed_execution": {"settings": self.settings}},
            },
        }
        (directory / "init/native-config.json").write_text(json.dumps(self.config))
        self.managed = {
            "terminal": self.config["terminal"],
            "agent": {"max_turns": 12},
            "plugins": self.config["plugins"],
        }
        self.write_managed()
        (directory / "trust/.env").write_text("TERMINAL_ENV=managed_execution\n")
        # Kubelet projects a writable file at terminationMessagePath: exercise
        # the actual bind-mount EBUSY behaviour, not an ordinary file.
        self.result = directory / "init-result.json"
        self.result.touch(mode=0o600)
        os.chown(self.result, 1000, 1000)

    def write_managed(self):
        (self.root / "trust/config.yaml").write_text(json.dumps(self.managed))

    def common(self):
        """Arguments every container of the instance shares."""
        arguments = [
            "--network",
            "none",
            "--read-only",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--user",
            "1000:1000",
            "--tmpfs",
            "/tmp:mode=1777",
            "--env",
            "PYTHONDONTWRITEBYTECODE=1",
            "--env",
            "HERMES_MANAGED_DIR=/run/hermes/execution-ca",
        ]
        for part, target in MOUNTS:
            mount = f"type=bind,src={self.root / part},dst={target}"
            if part in READ_ONLY:
                mount += ",readonly"
            arguments += ["--mount", mount]
        return arguments

    def initialize(self, *, extra=()):
        """Runs the installed initializer as the Pod's init container does."""
        docker(
            [
                "run",
                "--rm",
                *self.common(),
                "--mount",
                f"type=bind,src={self.root / 'init'},dst=/run/hermes/init,readonly",
                "--mount",
                f"type=bind,src={self.result},dst=/tmp/native-init-result.json",
                *extra,
                "--entrypoint",
                PYTHON,
                image(),
                "/usr/local/lib/personal-init.py",
                "--context",
                "/run/hermes/owner/context.json",
            ],
            timeout=60,
        )


class Instance:
    """The running native supervisor; read and inspect only through `check`."""

    def __init__(self, layout: Layout, cleanup: Cleanup):
        self.layout = layout
        self.run = new_run()
        self.name = f"personal-hermes-native-{self.run}"
        self.start(cleanup)

    def start(self, cleanup: Cleanup):
        docker(
            [
                "run",
                "-d",
                "--name",
                self.name,
                "--label",
                f"agent-platform.io/fixture={NATIVE_FIXTURE}",
                "--label",
                f"{RUN}={self.run}",
                *self.layout.common(),
                "--env",
                f"HERMES_DASHBOARD_OIDC_ISSUER={ISSUER}",
                "--env",
                "HERMES_DASHBOARD_OIDC_CLIENT_ID=native-hermes",
                "--env",
                "HERMES_DASHBOARD_PUBLIC_URL=https://hermes.example.org",
                "--env",
                "HERMES_PERSONAL_OIDC_CA_FILE=/run/hermes/execution-ca/oidc-ca.crt",
                "--entrypoint",
                PYTHON,
                image(),
                "/usr/local/lib/personal-run.py",
            ]
        )
        cleanup.add(
            self.name,
            lambda: remove_fixture(self.name, NATIVE_FIXTURE, self.run),
        )

    def running(self):
        state = docker(["inspect", "--format", "{{.State.Running}}", self.name])
        return state.strip() == b"true"

    def check(self, script, *, timeout=10):
        """Runs `script` inside the instance; it must exit cleanly."""
        return docker(["exec", self.name, PYTHON, "-c", script], timeout=timeout)

    def wait_ready(self, script, *, seconds=90):
        """Retries `script` until the instance is ready; a supervisor that
        exits is a failure at once."""
        deadline = time.monotonic() + seconds
        while True:
            if not self.running():
                raise CommandFailed("the native supervisor exited before readiness")
            try:
                self.check(script)
                return
            except CommandFailed:
                if time.monotonic() >= deadline:
                    raise CommandFailed("native startup readiness deadline") from None
                time.sleep(1)

    def kill_and_restart(self):
        docker(["kill", "--signal", "KILL", self.name], timeout=15)
        docker(["start", self.name], timeout=30)
