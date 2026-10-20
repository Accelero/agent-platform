"""Run bounded, resumable enrollment without reconciling live user preferences."""

from __future__ import annotations

import argparse
import base64
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid
from contextlib import contextmanager
from datetime import UTC, datetime
from pathlib import Path

import kubernetes.client as kube
import kubernetes.config

TIMEOUT = (5, 30)
_DECODE_ERRORS = (ValueError, TypeError)
_IDENTITY_ERRORS = (OSError, ValueError)
_CONNECTION_ERRORS = (urllib.error.URLError, TimeoutError, ConnectionError)


def native(config: dict, action: str, **values: object) -> dict:
    env = dict(os.environ)
    url = config["databaseUrl"]
    for key, value in env.items():
        url = url.replace("$(" + key + ")", value)
    if "$(" in url:
        raise RuntimeError("Unresolved database credential.")
    env["DATABASE_URL"] = url
    result = subprocess.run(
        ["/usr/local/bin/multica-bootstrap"],
        input=json.dumps(
            {"action": action, "installation": config["installation"], **values}
        ),
        capture_output=True,
        text=True,
        timeout=35,
        env=env,
    )
    if result.returncode:
        raise RuntimeError("Native bootstrap failed; private details suppressed.")
    return json.loads(result.stdout)


class Cluster:
    def __init__(self, config: dict) -> None:
        self.config = config
        self.namespace = config["namespace"]
        self.core = kube.CoreV1Api()
        self.apps = kube.AppsV1Api()
        self.leases = kube.CoordinationV1Api()

    def secret(self, name: str, namespace: str | None = None) -> dict:
        try:
            item = self.core.read_namespaced_secret(
                name, namespace or self.namespace, _request_timeout=TIMEOUT
            )
        except kube.ApiException as error:
            if error.status == 404:
                return {}
            raise
        return {
            key: base64.b64decode(value).decode()
            for key, value in (item.data or {}).items()
        }

    def save(self, name: str, data: dict, namespace: str | None = None) -> None:
        namespace = namespace or self.namespace
        body = {
            "metadata": {"name": name, "labels": self.config.get("labels", {})},
            "stringData": data,
        }
        try:
            existing = self.core.read_namespaced_secret(
                name, namespace, _request_timeout=TIMEOUT
            )
            labels = existing.metadata.labels or {}
            if any(
                labels.get(key) != value
                for key, value in self.config.get("labels", {}).items()
            ):
                raise RuntimeError("Enrollment Secret belongs to another installation.")
        except kube.ApiException as error:
            if error.status != 404:
                raise
            self.core.create_namespaced_secret(
                namespace, body, _request_timeout=TIMEOUT
            )
        else:
            self.core.patch_namespaced_secret(
                name, namespace, body, _request_timeout=TIMEOUT
            )

    def recovery(self) -> bool:
        """Require explicit restore completion, including namespace identity checks."""
        gate = self.core.read_namespaced_config_map(
            self.config["recoveryGate"], self.namespace, _request_timeout=TIMEOUT
        )
        record = json.loads(gate.data["record.json"])
        for namespace, expected in record["namespaceUIDs"].items():
            actual = self.core.read_namespace(namespace, _request_timeout=TIMEOUT)
            if actual.metadata.uid != expected:
                raise RuntimeError("Recovery namespace identity changed.")
        if record["installation"] != self.config["installation"] or not record["ready"]:
            raise RuntimeError("Recovery prerequisite is incomplete.")
        return record["fresh"] is True

    @contextmanager
    def lock(self):
        """Share the installation lease with backup/restore administrator Jobs."""
        now = datetime.now(UTC)
        name = self.config["leaseName"]
        body = kube.V1Lease(
            metadata=kube.V1ObjectMeta(name=name),
            spec=kube.V1LeaseSpec(
                holder_identity=os.environ["POD_UID"],
                acquire_time=now,
                renew_time=now,
                lease_duration_seconds=1500,
            ),
        )
        deadline = time.monotonic() + 180
        while True:
            try:
                old = self.leases.read_namespaced_lease(
                    name, self.namespace, _request_timeout=TIMEOUT
                )
            except kube.ApiException as error:
                if error.status != 404:
                    raise
                old = None
            if old:
                start = old.spec.renew_time or old.spec.acquire_time
                if (
                    start is None
                    or (now - start).total_seconds() <= old.spec.lease_duration_seconds
                ):
                    if time.monotonic() >= deadline:
                        raise RuntimeError(
                            "Another installation operation holds the lease."
                        )
                    time.sleep(2)
                    now = datetime.now(UTC)
                    continue
                body.metadata.resource_version = old.metadata.resource_version
            try:
                lease = (
                    self.leases.replace_namespaced_lease(
                        name, self.namespace, body, _request_timeout=TIMEOUT
                    )
                    if old
                    else self.leases.create_namespaced_lease(
                        self.namespace, body, _request_timeout=TIMEOUT
                    )
                )
                break
            except kube.ApiException as error:
                if error.status != 409 or time.monotonic() >= deadline:
                    raise
        try:
            yield
        finally:
            self.leases.delete_namespaced_lease(
                name,
                self.namespace,
                body=kube.V1DeleteOptions(
                    preconditions=kube.V1Preconditions(uid=lease.metadata.uid)
                ),
                _request_timeout=TIMEOUT,
            )

    def replicas(self) -> dict:
        return {
            name: self.apps.read_namespaced_deployment(
                name, self.namespace, _request_timeout=TIMEOUT
            ).spec.replicas
            for name in self.config["writers"]
        }

    def scale(self, name: str, replicas: int) -> None:
        self.apps.patch_namespaced_deployment_scale(
            name,
            self.namespace,
            {"spec": {"replicas": replicas}},
            _request_timeout=TIMEOUT,
        )
        if replicas == 0:
            deadline = time.monotonic() + 120
            while self.core.list_namespaced_pod(
                self.namespace,
                label_selector="app.kubernetes.io/name=" + name,
                _request_timeout=TIMEOUT,
            ).items:
                if time.monotonic() >= deadline:
                    raise RuntimeError("Native writer did not stop.")
                time.sleep(2)


def hook(config: dict, name: str) -> None:
    command = config["lifecycle"][name]
    result = subprocess.run(command, capture_output=True, timeout=600)
    if result.returncode:
        raise RuntimeError("Recovery lifecycle command failed; state retained.")


class Board:
    def __init__(self, config: dict, token: str, workspace: str) -> None:
        self.url = config["serverUrl"]
        self.token = token
        self.workspace = workspace

        # Do not forward credentials across redirects or environment proxies.
        class NoRedirect(urllib.request.HTTPRedirectHandler):
            def redirect_request(self, req, fp, code, msg, headers, newurl):
                return None

        self.opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({}), NoRedirect()
        )

    def request(
        self, path: str, payload: dict | None = None, method: str | None = None
    ) -> dict | list:
        request = urllib.request.Request(
            self.url + path,
            data=None if payload is None else json.dumps(payload).encode(),
            method=method,
            headers={
                "Authorization": "Bearer " + self.token,
                "X-Workspace-ID": self.workspace,
                "Content-Type": "application/json",
            },
        )
        with self.opener.open(request, timeout=15) as response:
            data = response.read(1048577)
        if len(data) > 1048576:
            raise RuntimeError("Unexpected board response size.")
        return json.loads(data)


def collection(value: dict | list, key: str) -> list:
    return value if isinstance(value, list) else value[key]


def install(path: Path, value: dict, *, force: bool) -> None:
    """Preserve every native preference while updating explicit auth fields."""
    old = json.loads(path.read_text()) if path.exists() else {}
    if not force and any(
        old.get(key, value[key]) != value[key]
        for key in ("token", "workspace_id", "server_url")
    ):
        raise RuntimeError(
            "Enrollment changed during interrupted bootstrap; use explicit force."
        )
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(".bootstrap-tmp")
    auth = {
        key: value[key] for key in ("token", "workspace_id", "server_url", "app_url")
    }
    temporary.write_text(json.dumps({**value, **old, **auth}))
    temporary.chmod(0o600)
    temporary.replace(path)


def enroll(config: dict, cluster: Cluster, *, force: bool) -> str:
    profile_name = config.get("profile", "")
    if profile_name and (
        not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]{0,63}", profile_name)
        or profile_name in {"default", "custom"}
    ):
        raise ValueError("Invalid bootstrap profile")
    with cluster.lock():
        fresh = cluster.recovery()
        deadline = time.monotonic() + 180
        while True:
            try:
                status = native(config, "inspect")
                break
            except RuntimeError:
                if time.monotonic() >= deadline:
                    raise
                time.sleep(2)
        saved = cluster.secret(config["onboardingSecret"])
        try:
            journal = json.loads(saved.get("bootstrap.json", "{}"))
        except _DECODE_ERRORS:
            if status["state"] != "initialized" or force:
                raise
            journal = {}
        if (
            status["state"] == "initialized"
            and not force
            and not journal.get("pending")
        ):
            return "initialized: enrollment unchanged"
        if journal.get("pending") and journal.get("mutation_complete"):
            hook(config, "checkpoint")
            journal["pending"] = False
            cluster.save(
                config["onboardingSecret"], {"bootstrap.json": json.dumps(journal)}
            )
            return "recovered: enrollment checkpoint verified"
        if (
            status["state"] == "empty"
            and not fresh
            and not (journal.get("pending") and journal.get("initial_fresh"))
        ):
            raise RuntimeError("An empty restored database is not a new installation.")
        if status["state"] == "pending" and not journal.get("pending") and not force:
            raise RuntimeError(
                "Interrupted credentials are missing; explicit repair required."
            )
        if not journal.get("pending"):
            tokens = native(config, "generate")
            previous = json.loads(saved.get("daemon.json", "{}"))
            journal = {
                **tokens,
                "pending": True,
                "initial_fresh": fresh,
                "force": force,
                "previous": previous.get("token", ""),
                "previous_profile": json.loads(saved.get("profile.json", "{}")).get(
                    "token", ""
                ),
                "workspace": previous.get("workspace_id", ""),
                "replicas": cluster.replicas(),
            }
            cluster.save(
                config["onboardingSecret"], {"bootstrap.json": json.dumps(journal)}
            )
        elif force:
            # Resume the pending credential pair rather than orphaning a new pair.
            journal["force"] = True
        if journal["force"] and status["state"] != "empty":
            # Native Multica would regenerate a missing/corrupt daemon ID. Refuse
            # before mutation rather than create a replacement runtime by accident.
            daemon_id = Path(config["home"]) / ".multica/daemon.id"
            try:
                uuid.UUID(daemon_id.read_text().strip())
            except _IDENTITY_ERRORS as error:
                raise RuntimeError(
                    "Restore the original native daemon identity before repair."
                ) from error
        hook(config, "before")
        identity = native(
            config,
            "ensure",
            email=config["email"],
            name=config["workspace"]["name"],
            slug=config["workspace"]["slug"],
            prefix=config["workspace"]["issuePrefix"],
            daemon_id=(Path(config["home"]) / ".multica/daemon.id").read_text().strip()
            if (Path(config["home"]) / ".multica/daemon.id").exists()
            else "",
            **{
                key: journal[key]
                for key in ("daemon", "profile", "previous", "workspace", "force")
            },
        )
        workspace = identity["workspace"]
        server = {
            "server_url": config["serverUrl"],
            "app_url": config["publicUrl"],
            "workspace_id": workspace,
        }
        daemon = {
            **json.loads(saved.get("daemon.json", "{}")),
            **server,
            "token": journal["daemon"],
        }
        profile = {
            **json.loads(saved.get("profile.json", "{}")),
            **server,
            "token": journal["profile"],
        }
        daemon.setdefault("device_name", "hermes-stack")
        daemon.setdefault("runtime_name", "Hermes container")
        daemon.setdefault("max_concurrent_tasks", 1)
        home = Path(config["home"])
        # The before hook has fenced dispatch, drained work and stopped writers.
        install(home / ".multica/config.json", daemon, force=journal["force"])
        if profile_name:
            profile_path = home / "profiles" / profile_name / ".multica/config.json"
            install(profile_path, profile, force=journal["force"])
            profile = json.loads(profile_path.read_text())
        daemon = json.loads((home / ".multica/config.json").read_text())
        cluster.save(
            config["onboardingSecret"],
            {"daemon.json": json.dumps(daemon), "profile.json": json.dumps(profile)},
        )
        cluster.save(
            config["profileSecret"],
            {"config.json": json.dumps(profile)},
            config["executionNamespace"],
        )
        cluster.scale("multica-backend", 1)
        cluster.scale("hermes", 1)
        board = Board(config, journal["profile"], workspace)
        deadline = time.monotonic() + 180
        while True:
            try:
                runtimes = collection(board.request("/api/runtimes"), "runtimes")
                matches = [
                    item
                    for item in runtimes
                    if item.get("provider") == "hermes"
                    and item.get("status") == "online"
                ]
                if len(matches) > 1:
                    raise RuntimeError(
                        "Ambiguous Hermes runtime; explicit migration required."
                    )
                if len(matches) == 1:
                    break
            except _CONNECTION_ERRORS:
                pass
            if time.monotonic() >= deadline:
                raise RuntimeError("Hermes did not reconnect within its deadline.")
            time.sleep(2)
        # Public lets every workspace member bind agents to the shared Hermes runtime.
        visibility = config.get("runtimeVisibility", "private")
        if visibility not in {"private", "public"}:
            raise ValueError("Invalid runtime visibility")
        if matches[0].get("visibility", "private") != visibility:
            board.request(
                "/api/runtimes/" + matches[0]["id"],
                {"visibility": visibility},
                method="PATCH",
            )
        agents = collection(board.request("/api/agents"), "agents")
        existing = [
            item
            for item in agents
            if item.get("runtime_id") == matches[0]["id"]
            and not item.get("archived_at")
        ]
        # Runtime-only installations leave agent creation to Multica users.
        if not existing and config.get("createAgent", True):
            seed = dict(config["agent"])
            if config.get("agentSeedFile"):
                seed = json.loads(Path(config["agentSeedFile"]).read_text())
            board.request(
                "/api/agents",
                {
                    **seed,
                    "runtime_id": matches[0]["id"],
                    "custom_args": ["-p", profile_name] if profile_name else [],
                },
            )
        native(
            config,
            "complete",
            user=identity["user"],
            workspace=workspace,
            previous=journal["previous"],
            previous_profile=journal.get("previous_profile", ""),
            daemon=journal["daemon"],
            profile=journal["profile"],
        )
        hook(config, "after")
        for name, count in journal["replicas"].items():
            cluster.scale(name, count)
        # A restored checkpoint with this journal only needs checkpoint completion,
        # never credential installation or dispatch resumption.
        journal["mutation_complete"] = True
        cluster.save(
            config["onboardingSecret"], {"bootstrap.json": json.dumps(journal)}
        )
        hook(config, "checkpoint")
        journal["pending"] = False
        cluster.save(
            config["onboardingSecret"], {"bootstrap.json": json.dumps(journal)}
        )
        return "enrolled: Hermes connected and recovery checkpoint verified"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--config", type=Path, default=Path("/etc/platform-bootstrap/config.json")
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Explicitly repair enrollment; preserve native identities and settings.",
    )
    args = parser.parse_args()
    os.umask(0o077)
    try:
        kubernetes.config.load_incluster_config()
        config = json.loads(args.config.read_text())
        print(enroll(config, Cluster(config), force=args.force))
    except Exception:
        sys.stderr.write(
            "Platform bootstrap failed; private details suppressed. "
            "State retained for retry.\n"
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
