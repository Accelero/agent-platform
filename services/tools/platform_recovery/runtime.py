"""Create and restore verified encrypted checkpoints in administrator Jobs."""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
import shutil
import sqlite3
import stat
import sys
import time
import uuid
from contextlib import AbstractContextManager, closing
from pathlib import Path
from typing import TYPE_CHECKING

import kubernetes.client as kube
import kubernetes.config
import yaml

from platform_recovery.commands import run
from platform_recovery.contract import resolve, validate_payload
from platform_recovery.port import Repository

if TYPE_CHECKING:
    pass

REQUEST_TIMEOUT = (5, 30)
NOT_FOUND = 404
CONFLICT = 409
MAX_RECORD_BYTES = 65_536
STAGING = Path("/staging")
DATASETS = Path("/datasets")
# A tuple constant keeps the Python 3.11 tools image's except syntax.
DATABASE_WAIT_ERRORS = (RuntimeError, TimeoutError)
SQLITE_HEADER = b"SQLite format 3\x00"
SQLITE_SIDECARS = ("-wal", "-shm", "-journal")
# Checkpoints record their dataset selection version; schema 1 held every dataset.
MANIFEST_SCHEMA = 3
# A checkpoint holds the copied payload and its decrypted readback at once.
HEADROOM_FACTOR = 1.1
HEADROOM_RESERVE = 512 * 1024**2
RUNTIME_ERRORS = (
    OSError,
    ValueError,
    TypeError,
    KeyError,
    RuntimeError,
    kube.ApiException,
    yaml.YAMLError,
)


class InsufficientSpaceError(RuntimeError):
    """A checkpoint would exhaust staging space; reported without private data."""


def inventory(root: Path, allowed_links: tuple[Path, ...] = ()) -> dict[str, dict]:
    """Hash ordinary files, preserve safe links, and reject special filesystem nodes."""
    result = {}
    for directory, subdirs, files in os.walk(root, followlinks=False):
        for name in sorted([*subdirs, *files]):
            path = Path(directory) / name
            relative = str(path.relative_to(root))
            metadata = path.lstat()
            if stat.S_ISSOCK(metadata.st_mode):
                continue
            entry = {"mode": stat.S_IMODE(metadata.st_mode)}
            if path.is_symlink():
                entry.update(kind="link", target=str(path.readlink()))
                target = Path(os.path.normpath(path.parent / path.readlink()))
                if not any(
                    target.is_relative_to(parent) for parent in (root, *allowed_links)
                ):
                    msg = "A durable dataset contains an external symlink."
                    raise ValueError(msg)
            elif path.is_file():
                with path.open("rb") as data:
                    entry.update(
                        kind="file",
                        sha256=hashlib.file_digest(data, "sha256").hexdigest(),
                    )
            elif path.is_dir():
                entry.update(kind="directory")
            else:
                msg = "A durable dataset contains a socket or special file."
                raise ValueError(msg)
            result[relative] = entry
    return result


def link_roots(datasets: Path, names: list[str]) -> tuple[Path, ...]:
    """Allow native overlay aliases only into datasets that this snapshot includes."""
    return (
        Path("/opt/data"),
        Path("/opt/platform/skills"),
        *(datasets / name for name in names),
    )


def selection(config: dict) -> tuple[int, list[str]]:
    """Return the newest versioned checkpoint selection.

    Excluded datasets, such as runtime-manager bookkeeping, start empty after a
    restore. Older listed versions stay restorable.
    """
    versions = config["backup_selections"]
    version = max(int(key) for key in versions)
    return version, list(versions[str(version)])


def checkpoint_datasets(config: dict, manifest: dict) -> list[str]:
    """Validate a checkpoint's dataset set against its recorded selection."""
    names = set(manifest.get("datasets", {}))
    if manifest.get("schema") == 1:
        valid = names == set(config["datasets"])
    elif manifest.get("schema") in (2, MANIFEST_SCHEMA):
        listed = config["backup_selections"].get(str(manifest.get("selection")))
        valid = listed is not None and names == set(listed)
    else:
        valid = False
    if not valid:
        msg = "The encrypted recovery manifest is incompatible or incomplete."
        raise ValueError(msg)
    return sorted(names)


def restored_datasets(config: dict, manifest: dict) -> list[str]:
    """Restore only datasets that the current selection still includes."""
    current = set(selection(config)[1])
    return [name for name in checkpoint_datasets(config, manifest) if name in current]


def payload_bytes(roots: list[Path]) -> int:
    """Sum regular file sizes, tolerating files removed during the walk."""
    total = 0
    for root in roots:
        for directory, _subdirs, files in os.walk(root, followlinks=False):
            for name in files:
                try:
                    metadata = (Path(directory) / name).lstat()
                except FileNotFoundError:
                    continue
                if stat.S_ISREG(metadata.st_mode):
                    total += metadata.st_size
    return total


def database_bytes() -> int:
    """Bound the custom-format dump by the database's on-disk size."""
    wait_for_database()
    return int(
        run(
            [
                "psql",
                "--no-psqlrc",
                "--tuples-only",
                "--no-align",
                "--set",
                "ON_ERROR_STOP=1",
                "--command",
                "SELECT pg_database_size(current_database())",
            ],
            timeout=30,
        ).strip(),
    )


def require_headroom(names: list[str]) -> None:
    """Refuse a checkpoint before copying when staging cannot hold it.

    The copied payload and its decrypted readback coexist, so twice the
    datasets and database must fit, with a margin for restic's cache.
    """
    payload = payload_bytes([DATASETS / name for name in names]) + database_bytes()
    needed = int(2 * payload * HEADROOM_FACTOR) + HEADROOM_RESERVE
    free = shutil.disk_usage(STAGING).free
    if free < needed:
        gib = 1024**3
        msg = (
            f"Checkpoint needs {needed / gib:.1f} GiB of staging space; "
            f"{free / gib:.1f} GiB is free."
        )
        raise InsufficientSpaceError(msg)


def ignore_sockets(directory: str, names: list[str]) -> list[str]:
    """Leave process communication endpoints out of durable file copies."""
    return [
        name
        for name in names
        if stat.S_ISSOCK((Path(directory) / name).lstat().st_mode)
    ]


def sqlite_database(path: Path) -> bool:
    """Recognize SQLite databases by their file header, not their name."""
    try:
        with path.open("rb") as data:
            return data.read(len(SQLITE_HEADER)) == SQLITE_HEADER
    except OSError:
        return False


def copy_sqlite(source: Path, target: Path) -> None:
    """Take a transactionally consistent copy while the application keeps writing."""
    with (
        closing(
            sqlite3.connect(f"file:{source}?mode=ro", uri=True, timeout=30),
        ) as reader,
        closing(sqlite3.connect(target)) as writer,
    ):
        reader.backup(writer)
    shutil.copystat(source, target)


def copy_file(path: Path, destination: Path) -> None:
    """Copy one regular file; SQLite sidecars are covered by the database copy."""
    base = next(
        (path.name[: -len(end)] for end in SQLITE_SIDECARS if path.name.endswith(end)),
        None,
    )
    if base and sqlite_database(path.with_name(base)):
        return
    try:
        if sqlite_database(path):
            copy_sqlite(path, destination)
        else:
            shutil.copy2(path, destination)
    except FileNotFoundError:
        return


def copy_live(source: Path, target: Path) -> None:
    """Copy one dataset without stopping writers.

    SQLite databases use the online backup API, which already includes their
    write-ahead log, so their sidecars are skipped. Other files are copied as
    they are, giving a crash-consistent snapshot. Files removed during the walk
    are skipped; sockets are omitted and special files are refused.
    """
    target.mkdir(parents=True)
    shutil.copystat(source, target)
    for directory, subdirs, files in os.walk(source, followlinks=False):
        relative = Path(directory).relative_to(source)
        for name in sorted([*subdirs, *files]):
            path = Path(directory) / name
            destination = target / relative / name
            try:
                metadata = path.lstat()
            except FileNotFoundError:
                continue
            if stat.S_ISSOCK(metadata.st_mode):
                continue
            if stat.S_ISLNK(metadata.st_mode):
                destination.symlink_to(path.readlink())
                if name in subdirs:
                    subdirs.remove(name)
            elif stat.S_ISDIR(metadata.st_mode):
                destination.mkdir()
                shutil.copystat(path, destination)
            elif stat.S_ISREG(metadata.st_mode):
                copy_file(path, destination)
            else:
                msg = "A durable dataset contains a socket or special file."
                raise ValueError(msg)


def require_empty(root: Path) -> None:
    """Refuse to adopt or overwrite initialized user files during restore."""
    if any(root.iterdir()):
        msg = "Restore destination is nonempty; explicit migration is required."
        raise RuntimeError(msg)


class Cluster:
    """Serialize checkpoints and preserve the exact workload restart state."""

    def __init__(self, config: dict) -> None:
        """Use the narrowly scoped administrator Job service account."""
        kubernetes.config.load_incluster_config()
        self.config = config
        self.core = kube.CoreV1Api()
        self.apps = kube.AppsV1Api()
        self.coordination = kube.CoordinationV1Api()
        self.namespace = config["namespace"]
        value = self.core.read_namespace(
            self.namespace,
            _request_timeout=REQUEST_TIMEOUT,
        )
        if value.metadata.uid != config["namespace_uid"]:
            msg = "The installation namespace identity changed."
            raise RuntimeError(msg)
        if config.get("execution_namespace_uid"):
            execution = self.core.read_namespace(
                config["execution_namespace"],
                _request_timeout=REQUEST_TIMEOUT,
            )
            if execution.metadata.uid != config["execution_namespace_uid"]:
                msg = "The worker namespace identity changed."
                raise RuntimeError(msg)
        if config.get("database_uid"):
            binding = config["database_resource"]
            database = kube.CustomObjectsApi().get_namespaced_custom_object(
                binding["group"],
                binding["version"],
                self.namespace,
                binding["plural"],
                binding["name"],
                _request_timeout=REQUEST_TIMEOUT,
            )
            if database["metadata"]["uid"] != config["database_uid"]:
                msg = "The database identity changed; checkpoint refused."
                raise RuntimeError(msg)

    def lock(self) -> AbstractContextManager[None]:
        """Share the exact bootstrap operation lease across the full checkpoint."""
        from platform_recovery.lease import operation_lock

        return operation_lock(self.coordination, self.namespace)

    def record(self, name: str, value: dict) -> None:
        """Store only nonsecret lifecycle progress in the installation namespace."""
        body = {"metadata": {"name": name}, "data": {"record.json": json.dumps(value)}}
        try:
            self.core.patch_namespaced_config_map(
                name,
                self.namespace,
                body,
                _request_timeout=REQUEST_TIMEOUT,
            )
        except kube.ApiException as error:
            if error.status != NOT_FOUND:
                raise
            self.core.create_namespaced_config_map(
                self.namespace,
                body,
                _request_timeout=REQUEST_TIMEOUT,
            )

    def read_record(self, name: str) -> dict | None:
        """Separate confirmed absence from failed recovery-state reads."""
        try:
            value = self.core.read_namespaced_config_map(
                name,
                self.namespace,
                _request_timeout=REQUEST_TIMEOUT,
            )
        except kube.ApiException as error:
            if error.status == NOT_FOUND:
                return None
            raise
        return json.loads(value.data["record.json"])

    def replicas(self) -> dict[str, int]:
        """Retain absent partial-install workloads as absent, never default to one."""
        result = {}
        for name in self.config["writers"]:
            try:
                value = self.apps.read_namespaced_deployment(
                    name,
                    self.namespace,
                    _request_timeout=REQUEST_TIMEOUT,
                )
            except kube.ApiException as error:
                if error.status != NOT_FOUND:
                    raise
            else:
                result[name] = value.spec.replicas
        return result

    def scale(self, replicas: dict[str, int]) -> None:
        """Change only explicitly selected installation writers."""
        for name, count in replicas.items():
            self.apps.patch_namespaced_deployment_scale(
                name,
                self.namespace,
                {"spec": {"replicas": count}},
                _request_timeout=REQUEST_TIMEOUT,
            )

    def secrets(self) -> list[dict]:
        """Capture selected credential data only into the encrypted checkpoint."""
        result = []
        for item in self.config["secrets"]:
            try:
                value = self.core.read_namespaced_secret(
                    item["name"],
                    item["namespace"],
                    _request_timeout=REQUEST_TIMEOUT,
                )
            except kube.ApiException as error:
                if error.status == NOT_FOUND and not item["required"]:
                    continue
                raise
            result.append(
                {
                    "name": item["name"],
                    "namespace": item["namespace"],
                    "type": value.type,
                    "data": value.data or {},
                },
            )
        return result

    def restore_secrets(self, values: list[dict]) -> None:
        """Restore only inventory entries selected by this installation."""
        allowed = {(item["namespace"], item["name"]) for item in self.config["secrets"]}
        required = {
            (item["namespace"], item["name"])
            for item in self.config["secrets"]
            if item["required"]
        }
        actual = {(item["namespace"], item["name"]) for item in values}
        if (
            not required <= actual
            or not actual <= allowed
            or len(actual) != len(values)
        ):
            msg = "The recovered credential inventory is incomplete or foreign."
            raise ValueError(msg)
        for item in values:
            body = {
                "metadata": {
                    "name": item["name"],
                    "labels": {
                        self.config["ownership_label"]: self.config["installation"],
                    },
                },
                "type": item["type"],
                "data": item["data"],
            }
            try:
                self.core.create_namespaced_secret(
                    item["namespace"],
                    body,
                    _request_timeout=REQUEST_TIMEOUT,
                )
            except kube.ApiException as error:
                if error.status != CONFLICT:
                    raise
                # A restore must precede credential generation and workloads.
                existing = self.core.read_namespaced_secret(
                    item["name"],
                    item["namespace"],
                    _request_timeout=REQUEST_TIMEOUT,
                )
                if existing.data != item["data"]:
                    msg = "Restore would overwrite an existing credential."
                    raise RuntimeError(msg) from None

    def wait_stopped(self) -> None:
        """Require all selected controllers and worker processes to exit."""
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            controllers = self.core.list_namespaced_pod(
                self.namespace,
                label_selector="app.kubernetes.io/instance="
                + self.config["release_name"],
                _request_timeout=REQUEST_TIMEOUT,
            ).items
            workers = self.core.list_namespaced_pod(
                self.config["execution_namespace"],
                label_selector="app.kubernetes.io/managed-by=agent-platform-runtime-manager",
                _request_timeout=REQUEST_TIMEOUT,
            ).items
            for pod in workers:
                self.core.delete_namespaced_pod(
                    pod.metadata.name,
                    self.config["execution_namespace"],
                    body=kube.V1DeleteOptions(
                        preconditions=kube.V1Preconditions(uid=pod.metadata.uid),
                    ),
                    _request_timeout=REQUEST_TIMEOUT,
                )
            if not controllers and not workers:
                return
            time.sleep(2)
        msg = "Agent writers did not stop within their deadline."
        raise TimeoutError(msg)


def fence(path: Path, *, pause: bool) -> None:
    """Publish maintenance after all in-flight backend claim transactions finish."""
    with path.with_suffix(".lock").open("a") as lock:
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                time.sleep(0.1)
            else:
                break
        else:
            msg = "Task claim maintenance fence did not become available."
            raise TimeoutError(msg)
        if pause:
            path.write_text("paused\n")
        else:
            path.unlink(missing_ok=True)


def publish(
    cluster: Cluster,
    repository: Repository,
    paths: tuple[Path, Path],
    datasets: dict,
    *,
    final: bool,
) -> dict:
    """Dump the database, then upload and verify one complete encrypted snapshot."""
    config = cluster.config
    root, verify = paths
    wait_for_database()
    run(
        ["pg_dump", "--format=custom", "--file", str(root / "database.dump")],
        timeout=180,
    )
    (root / "secrets.json").write_text(json.dumps(cluster.secrets()))
    manifest = {
        "schema": MANIFEST_SCHEMA,
        "recovery_api": 1,
        "inventory_sha256": config["inventory_sha256"],
        "platform_version": config["recovery"]["platformVersion"],
        "selection": selection(config)[0],
        "installation": config["installation"],
        "checkpoint": uuid.uuid4().hex,
        "database_major": config["database_major"],
        "datasets": datasets,
        "artifacts": config["artifacts"],
        "seed_sha256": config["seed_sha256"],
        "final": final,
        "namespace_uid": config["namespace_uid"],
        "database_uid": config["database_uid"],
        "configuration_sha256": config["configuration_sha256"],
    }
    for name, field in (
        ("database.dump", "database_sha256"),
        ("secrets.json", "secrets_sha256"),
    ):
        with (root / name).open("rb") as data:
            manifest[field] = hashlib.file_digest(data, "sha256").hexdigest()
    # Retention runs before adding the new snapshot and again once it is verified.
    previous = repository.pointer()
    repository.retain(previous["snapshot"] if previous else None)
    record = repository.checkpoint(root, manifest, verify)
    cluster.record("agent-state-checkpoint", {**record, "final": final})
    repository.retain(record["snapshot"])
    return record


def staging() -> tuple[Path, Path]:
    """Start every checkpoint from empty plaintext staging directories."""
    paths = (STAGING / "checkpoint", STAGING / "readback")
    for directory in paths:
        if directory.exists():
            shutil.rmtree(directory)
        directory.mkdir(parents=True)
    return paths


def release_cutoff(cluster: Cluster, maintenance: Path) -> None:
    """End a final cutoff left by a pre-upgrade or failed checkpoint.

    An upgrade stops writers with a final checkpoint; the following ordinary
    checkpoint restores them and dispatch, as the cold implementation did.
    """
    pending = cluster.read_record("agent-state-pending")
    if not pending or (pending.get("complete") and not pending.get("final")):
        return
    if not pending.get("was_paused"):
        fence(maintenance, pause=False)
    cluster.scale(pending["replicas"])
    cluster.record(
        "agent-state-pending",
        {**pending, "complete": True, "final": False},
    )


def live_backup(config: dict) -> dict:
    """Checkpoint while agents keep running; nothing is paused or drained."""
    cluster = Cluster(config)
    repository = Repository(config)
    maintenance = DATASETS / config["recovery"]["fence"]
    names = selection(config)[1]
    with cluster.lock():
        try:
            paths = staging()
            require_headroom(names)
            datasets = {}
            for name in names:
                target = paths[0] / "datasets" / name
                copy_live(DATASETS / name, target)
                datasets[name] = inventory(
                    target,
                    link_roots(paths[0] / "datasets", names),
                )
            record = publish(cluster, repository, paths, datasets, final=False)
            # No plaintext state remains after a successful checkpoint.
            for directory in paths:
                shutil.rmtree(directory)
            return record
        finally:
            release_cutoff(cluster, maintenance)


def backup(config: dict, *, final: bool) -> dict:
    """Periodic checkpoints run live; a final one force-stops writers first."""
    if not final:
        return live_backup(config)
    cluster = Cluster(config)
    repository = Repository(config)
    maintenance = DATASETS / config["recovery"]["fence"]
    names = selection(config)[1]
    with cluster.lock():
        # Check space before recording or interrupting anything.
        paths = staging()
        require_headroom(names)
        pending = cluster.read_record("agent-state-pending")
        # A completed final cutoff left writers stopped, for example before a
        # failed bootstrap retries; keep the replicas saved before that cutoff.
        prior = (
            pending["replicas"]
            if pending and (not pending.get("complete") or pending.get("final"))
            else cluster.replicas()
        )
        previous_pause = (
            pending["was_paused"]
            if pending and pending.get("complete") and pending.get("final")
            else maintenance.exists()
        )
        cluster.record(
            "agent-state-pending",
            {"replicas": prior, "was_paused": previous_pause},
        )
        completed = False
        try:
            fence(maintenance, pause=True)
            # Teardown does not wait for running work: it is interrupted and the
            # stopped state is captured exactly.
            cluster.scale(dict.fromkeys(prior, 0))
            cluster.wait_stopped()
            datasets = {}
            for name in names:
                source = DATASETS / name
                expected = inventory(source, link_roots(DATASETS, names))
                target = paths[0] / "datasets" / name
                shutil.copytree(source, target, symlinks=True, ignore=ignore_sockets)
                if (
                    inventory(target, link_roots(paths[0] / "datasets", names))
                    != expected
                ):
                    msg = "A durable dataset changed while writers were stopped."
                    raise RuntimeError(msg)
                datasets[name] = expected
            record = publish(cluster, repository, paths, datasets, final=True)
            completed = True
            return record
        finally:
            if not completed:
                if not previous_pause:
                    fence(maintenance, pause=False)
                cluster.scale(prior)
            else:
                cluster.record(
                    "agent-state-pending",
                    {
                        "replicas": prior,
                        "complete": True,
                        "was_paused": previous_pause,
                        "final": True,
                    },
                )
                # No plaintext state remains after a successful checkpoint.
                for directory in paths:
                    shutil.rmtree(directory, ignore_errors=False)


def restore_files(config: dict) -> dict:
    """Restore files and credential identities before database/application startup."""
    cluster = Cluster(config)
    repository = Repository(config)
    with cluster.lock():
        initialized = cluster.read_record("agent-state-initialized")
        if initialized:
            if initialized.get("namespace_uid") != config["namespace_uid"]:
                msg = "Initialization record belongs to another namespace."
                raise RuntimeError(msg)
            return {"restored": bool(initialized.get("snapshot"))}
        partial = cluster.read_record("agent-state-restoring")
        record = repository.pointer()
        if partial and (
            record is None or partial.get("snapshot") != record["snapshot"]
        ):
            msg = "The recovery selection changed during a partial restore."
            raise RuntimeError(msg)
        for name in config["datasets"]:
            if partial is None:
                require_empty(DATASETS / name)
        if record is None:
            repository.initialize_empty()
            cluster.record(
                "agent-state-initialized",
                {"namespace_uid": config["namespace_uid"]},
            )
            return {"restored": False}
        target = STAGING / "restore"
        if target.exists():
            shutil.rmtree(target)
        target.mkdir(parents=True)
        checkpoint, manifest = repository.verify(record, target)
        cluster.record("agent-state-restoring", record)
        cluster.restore_secrets(json.loads((checkpoint / "secrets.json").read_text()))
        copy_restored_files(config, checkpoint, manifest)
        shutil.copyfile(checkpoint / "database.dump", STAGING / "database.dump")
        cluster.record("agent-state-restored", {**record, "database_restored": False})
        cluster.record(
            "agent-state-initialized",
            {"namespace_uid": config["namespace_uid"], "snapshot": record["snapshot"]},
        )
        shutil.rmtree(target)
        return {"restored": True, "snapshot": record["snapshot"]}


def copy_restored_files(config: dict, checkpoint: Path, manifest: dict) -> None:
    """Retry a partial file restore only when existing files match the snapshot.

    Datasets outside the current selection, such as runtime-manager bookkeeping
    in checkpoints that predate selections, are left empty.
    """
    names = restored_datasets(config, manifest)
    for name in names:
        expected = manifest["datasets"][name]
        destination = DATASETS / name
        actual = inventory(destination, link_roots(DATASETS, names))
        if any(expected.get(key) != value for key, value in actual.items()):
            msg = "Partial restore contains foreign or changed files."
            raise RuntimeError(msg)
        # Matching links are already correct; copytree cannot recreate them.
        for relative, entry in actual.items():
            if entry["kind"] == "link":
                (destination / relative).unlink()
        shutil.copytree(
            checkpoint / "datasets" / name,
            destination,
            dirs_exist_ok=True,
            symlinks=True,
        )
        if inventory(destination, link_roots(DATASETS, names)) != expected:
            msg = "Restored native files did not match the selected checkpoint."
            raise RuntimeError(msg)


def wait_for_database(deadline_seconds: float = 180) -> None:
    """Wait until PostgreSQL accepts connections from this Job.

    A newly started Pod has no network access for its first seconds while the
    node programs its network policies, and a fresh database may still start.
    """
    deadline = time.monotonic() + deadline_seconds
    while True:
        try:
            run(["pg_isready", "--quiet", "--timeout=5"], timeout=15)
        except DATABASE_WAIT_ERRORS:
            if time.monotonic() >= deadline:
                msg = "The agent database did not accept connections in time."
                raise TimeoutError(msg) from None
            time.sleep(2)
        else:
            return


def _restore_database(config: dict) -> dict:
    """Refuse nonempty database targets, and never replay queued work on restore."""
    cluster = Cluster(config)
    record = cluster.read_record("agent-state-restored")
    if record is None or record.get("database_restored"):
        return {"database_restored": False}
    wait_for_database()
    result = run(
        [
            "psql",
            "--no-psqlrc",
            "--tuples-only",
            "--no-align",
            "--set",
            "ON_ERROR_STOP=1",
            "--command",
            "SELECT count(*) FROM pg_tables WHERE schemaname='public'",
        ],
        timeout=30,
    ).strip()
    if result != b"0":
        msg = "Database restore target is nonempty; operator recovery is required."
        raise RuntimeError(msg)
    # Extensions exist already, owned by the database superuser; the application
    # role cannot comment on them. Restore every other entry.
    contents = run(
        ["pg_restore", "--list", str(STAGING / "database.dump")],
        timeout=60,
    ).decode()
    selection = STAGING / "restore.list"
    selection.write_text(
        "".join(
            line
            for line in contents.splitlines(keepends=True)
            if " COMMENT - EXTENSION " not in line
        ),
    )
    sql_path = STAGING / "restore.sql"
    run(
        [
            "pg_restore",
            "--exit-on-error",
            "--no-owner",
            "--no-privileges",
            "--use-list",
            str(selection),
            "--file",
            str(sql_path),
            str(STAGING / "database.dump"),
        ],
        timeout=300,
    )
    selection.unlink()
    sanitization = (
        "UPDATE public.agent_task_queue SET status='cancelled', "
        "completed_at=now(), "
        "error='Interrupted by restore; review and rerun explicitly' "
        "WHERE status IN ('queued','dispatched','running',"
        "'deferred','waiting_local_directory'); "
        "UPDATE public.issue_wakeup SET enabled=false, next_fire_at=NULL "
        "WHERE enabled; "
        "UPDATE public.autopilot SET status='paused' WHERE status='active'; "
    )
    run(
        [
            "psql",
            "--no-psqlrc",
            "--set",
            "ON_ERROR_STOP=1",
            "--single-transaction",
            "--file",
            str(sql_path),
            "--command",
            sanitization,
        ],
        timeout=300,
    )
    sql_path.unlink()
    cluster.record(
        "agent-state-restored",
        {**record, "database_restored": True, "dispatch_paused": True},
    )
    Path(STAGING / "database.dump").unlink()
    return {"database_restored": True, "dispatch_paused": True}


def restore_database(config: dict) -> dict:
    """Serialize database import with every administrator operation."""
    with Cluster(config).lock():
        return _restore_database(config)


def verify_checkpoint(config: dict) -> dict:
    """Read back the selected complete payload without changing live datasets."""
    cluster = Cluster(config)
    repository = Repository(config)
    with cluster.lock():
        record = repository.pointer()
        if record is None:
            msg = "No completed recovery checkpoint exists."
            raise RuntimeError(msg)
        target = STAGING / "verification"
        if target.exists():
            shutil.rmtree(target)
        target.mkdir(parents=True)
        try:
            repository.verify(record, target)
        finally:
            shutil.rmtree(target)
    return {"verified": True, "snapshot": record["snapshot"]}


def resume(config: dict) -> dict:
    """Resume dispatch only through an explicit administrator operation."""
    cluster = Cluster(config)
    with cluster.lock():
        restored = cluster.read_record("agent-state-restored")
        if restored and not restored.get("database_restored"):
            msg = "Database recovery has not completed; dispatch stays paused."
            raise RuntimeError(msg)
        fence(DATASETS / config["recovery"]["fence"], pause=False)
        pending = cluster.read_record("agent-state-pending")
        if pending:
            cluster.scale(pending["replicas"])
        if restored:
            cluster.record(
                "agent-state-restored",
                {**restored, "dispatch_paused": False},
            )
    return {"dispatch_paused": False}


def main() -> int:
    """Emit status only; suppress credential-bearing library and command errors."""
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "operation",
        choices=(
            "backup",
            "final",
            "restore-files",
            "restore-database",
            "verify",
            "resume",
            "verify-payload",
            "inventory",
        ),
    )
    parser.add_argument(
        "--config",
        type=Path,
        default=Path("/etc/agent-state/config.json"),
    )
    args = parser.parse_args()
    try:
        if args.operation == "verify-payload":
            request = json.loads(sys.stdin.read(4 * 1024 * 1024))
            validate_payload(
                request["config"], request["record"], Path(request["checkpoint"])
            )
            sys.stdout.write('{"verified": true}\n')
            return 0
        config = resolve(json.loads(args.config.read_text()))
        if args.operation == "inventory":
            sys.stdout.write(
                json.dumps(
                    {"apiVersion": 1, "inventory_sha256": config["inventory_sha256"]}
                )
                + "\n"
            )
            return 0
        if args.operation in {"backup", "final"}:
            result = backup(config, final=args.operation == "final")
        elif args.operation == "restore-files":
            result = restore_files(config)
        elif args.operation == "verify":
            result = verify_checkpoint(config)
        elif args.operation == "resume":
            result = resume(config)
        else:
            result = restore_database(config)
    except InsufficientSpaceError as error:
        sys.stderr.write(str(error) + "\n")
        return 1
    except RUNTIME_ERRORS:
        sys.stderr.write("Agent state operation failed; private details suppressed.\n")
        return 1
    sys.stdout.write(json.dumps(result) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
