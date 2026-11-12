"""Exercise application recovery cutoffs without a deployment checkout."""

import os
import socket
import sqlite3
import sys
from collections.abc import Iterator
from contextlib import nullcontext
from pathlib import Path
from unittest.mock import ANY, MagicMock, call, patch

import pytest
from platform_recovery import runtime as state
from platform_recovery.contract import descriptor

pytestmark = pytest.mark.unit
PRIVATE = "fixture-private-credential"


def expect_equal(actual, expected):
    assert actual == expected


@pytest.fixture
def config():
    recovery = descriptor()
    return {
        "installation": "fixture",
        "namespace": "agents",
        "namespace_uid": "ns-uid",
        "execution_namespace": "agent-execution",
        "execution_namespace_uid": "exec-uid",
        "database_uid": "db-uid",
        "database_major": 17,
        "configuration_sha256": "config-hash",
        "seed_sha256": "seed-hash",
        "datasets": dict.fromkeys(recovery["datasets"], "1Gi"),
        "worker_datasets": ["hermes-tasks", "hermes-workspaces"],
        "backup_selections": recovery["selections"],
        "artifacts": {"tools": "fixture-image"},
        "recovery": recovery,
        "inventory_sha256": "fixture-inventory",
        "writers": recovery["writers"],
    }


def test_inventory_links_sockets_and_special_files(tmp_path: Path) -> None:
    """Native aliases survive, process sockets are omitted, and devices fail closed."""
    (tmp_path / "alias").symlink_to("/opt/data/auth.json")
    with socket.socket(socket.AF_UNIX) as endpoint:
        endpoint.bind(str(tmp_path / "process.sock"))
        actual = state.inventory(tmp_path, (Path("/opt/data"),))
    expect_equal(set(actual), {"alias"})
    with pytest.raises(ValueError, match="external symlink"):
        state.inventory(tmp_path)
    (tmp_path / "alias").unlink()
    os.mkfifo(tmp_path / "special")
    with pytest.raises(ValueError, match="special file"):
        state.inventory(tmp_path)


@pytest.fixture
def runtime(tmp_path: Path, config: dict) -> Iterator[tuple[MagicMock, MagicMock]]:
    """Use real file copies with mocked Kubernetes, PostgreSQL and object storage."""
    datasets = tmp_path / "datasets"
    staging = tmp_path / "staging"
    staging.mkdir()
    for name in config["datasets"]:
        (datasets / name).mkdir(parents=True)
        (datasets / name / "settings").write_text("preserved live state")
    cluster = MagicMock()
    cluster.config = config
    cluster.lock.return_value = nullcontext()
    cluster.read_record.return_value = None
    cluster.replicas.return_value = dict.fromkeys(config["writers"], 1)
    cluster.secrets.return_value = [{"token": PRIVATE}]
    repository = MagicMock()
    repository.pointer.return_value = None
    repository.checkpoint.return_value = {"snapshot": "a" * 64}

    def commands(args: list[str], **_kwargs: object) -> bytes:
        if args[0] == "pg_dump":
            Path(args[-1]).write_bytes(b"database-dump")
        return b"0"

    with (
        patch.object(state, "DATASETS", datasets),
        patch.object(state, "STAGING", staging),
        patch.object(state, "Cluster", return_value=cluster),
        patch.object(state, "Repository", return_value=repository),
        patch.object(state, "run", side_effect=commands),
    ):
        yield cluster, repository


def test_periodic_backup_runs_live_without_pausing_agents(
    config: dict,
    runtime: tuple[MagicMock, MagicMock],
) -> None:
    """Nightly checkpoints never fence, drain, stop or restart application writers."""
    cluster, repository = runtime
    state.backup(config, final=False)
    cluster.scale.assert_not_called()
    cluster.wait_stopped.assert_not_called()
    expect_equal(
        (state.DATASETS / "multica-uploads/.altiera-maintenance").exists(),
        expected=False,
    )
    manifest = repository.checkpoint.call_args.args[1]
    expect_equal(manifest["final"], expected=False)
    expect_equal((manifest["schema"], manifest["selection"]), (3, 1))
    expect_equal(set(manifest["datasets"]), set(config["datasets"]) - {"manager-state"})
    expect_equal(
        [entry.args[0] for entry in cluster.record.call_args_list],
        ["agent-state-checkpoint"],
    )
    expect_equal(list((state.STAGING).iterdir()), [])


def test_final_backup_force_stops_writers_without_waiting_for_work(
    config: dict,
    runtime: tuple[MagicMock, MagicMock],
) -> None:
    """Teardown stops every writer at once and leaves them stopped afterwards."""
    cluster, repository = runtime
    state.backup(config, final=True)
    expect_equal(
        [entry.args[0] for entry in cluster.scale.call_args_list],
        [dict.fromkeys(config["writers"], 0)],
    )
    cluster.wait_stopped.assert_called_once()
    expect_equal(repository.checkpoint.call_args.args[1]["final"], expected=True)
    expect_equal(
        (state.DATASETS / "multica-uploads/.altiera-maintenance").exists(),
        expected=True,
    )
    expect_equal(hasattr(state, "drain"), expected=False)


def test_repeated_final_cutoff_keeps_the_original_replicas(
    config: dict,
    runtime: tuple[MagicMock, MagicMock],
) -> None:
    """A retried cold operation must not record the writers it already stopped."""
    cluster, _repository = runtime
    running = dict.fromkeys(config["writers"], 1)
    cluster.read_record.return_value = {
        "replicas": running,
        "complete": True,
        "was_paused": False,
        "final": True,
    }
    cluster.replicas.return_value = dict.fromkeys(config["writers"], 0)
    state.backup(config, final=True)
    records = [
        entry.args[1]
        for entry in cluster.record.call_args_list
        if entry.args[0] == "agent-state-pending"
    ]
    expect_equal({record["replicas"] == running for record in records}, {True})


def test_checkpoints_exclude_manager_bookkeeping_by_versioned_selection(
    config: dict,
) -> None:
    """Runtime-manager state is not part of any listed selection."""
    version, names = state.selection(config)
    expect_equal(version, 1)
    expect_equal("manager-state" in names, expected=False)
    expect_equal(set(names) <= set(config["datasets"]), expected=True)


@pytest.mark.parametrize(
    "manifest",
    [
        {"schema": 2, "selection": 9, "datasets": {"hermes-home": {}}},
        {"schema": 2, "selection": 1, "datasets": {"hermes-home": {}}},
        {"schema": 1, "datasets": {"hermes-home": {}}},
        {"schema": 3, "selection": 1, "datasets": {}},
    ],
)
def test_unlisted_or_mismatched_selections_are_refused(
    config: dict,
    manifest: dict,
) -> None:
    """A checkpoint must match a listed selection or the legacy full set exactly."""
    with pytest.raises(ValueError, match="incompatible"):
        state.checkpoint_datasets(config, manifest)


def test_legacy_restore_leaves_manager_bookkeeping_empty(
    config: dict,
    runtime: tuple[MagicMock, MagicMock],
    tmp_path: Path,
) -> None:
    """Schema 1 checkpoints held every dataset; manager state now starts clean."""
    del runtime
    payload = tmp_path / "payload"
    manifest = {"schema": 1, "datasets": {}}
    for name in config["datasets"]:
        (state.DATASETS / name / "settings").unlink()
        source = payload / "datasets" / name
        source.mkdir(parents=True)
        (source / "file").write_text(name)
        manifest["datasets"][name] = state.inventory(source)
    state.copy_restored_files(config, payload, manifest)
    expect_equal(list((state.DATASETS / "manager-state").iterdir()), [])
    expect_equal((state.DATASETS / "hermes-home/file").read_text(), "hermes-home")


@pytest.mark.parametrize("final", [False, True])
def test_insufficient_headroom_refuses_before_copying_or_stopping(
    config: dict,
    runtime: tuple[MagicMock, MagicMock],
    *,
    final: bool,
) -> None:
    """A full staging disk fails early and leaves writers running."""
    cluster, repository = runtime
    usage = MagicMock(free=1024)
    with (
        patch.object(state.shutil, "disk_usage", return_value=usage),
        pytest.raises(state.InsufficientSpaceError, match="GiB is free"),
    ):
        state.backup(config, final=final)
    cluster.scale.assert_not_called()
    cluster.wait_stopped.assert_not_called()
    repository.checkpoint.assert_not_called()
    expect_equal(list((state.STAGING / "checkpoint").iterdir()), [])
    expect_equal(
        (state.DATASETS / "multica-uploads/.altiera-maintenance").exists(),
        expected=False,
    )


def test_headroom_counts_payload_twice_with_the_database(
    tmp_path: Path,
) -> None:
    """The copied payload and its readback coexist with the database dump."""
    datasets = tmp_path / "datasets"
    (datasets / "data").mkdir(parents=True)
    (datasets / "data/blob").write_bytes(b"x" * 1000)
    payload = 1000 + 500
    needed = int(2 * payload * state.HEADROOM_FACTOR) + state.HEADROOM_RESERVE
    with (
        patch.object(state, "DATASETS", datasets),
        patch.object(state, "STAGING", tmp_path),
        patch.object(state, "run", return_value=b"500\n"),
        patch.object(state.shutil, "disk_usage", return_value=MagicMock(free=needed)),
    ):
        state.require_headroom(["data"])
    with (
        patch.object(state, "DATASETS", datasets),
        patch.object(state, "STAGING", tmp_path),
        patch.object(state, "run", return_value=b"500\n"),
        patch.object(
            state.shutil,
            "disk_usage",
            return_value=MagicMock(free=needed - 1),
        ),
        pytest.raises(state.InsufficientSpaceError),
    ):
        state.require_headroom(["data"])


def test_space_failure_is_reported_without_suppression(
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """Operators see the space shortfall; other failures stay suppressed."""
    config_file = tmp_path / "config.json"
    config_file.write_text("{}")
    error = state.InsufficientSpaceError("Checkpoint needs 2.0 GiB; 1.0 GiB is free.")
    with (
        patch.object(sys, "argv", ["state", "backup", "--config", str(config_file)]),
        patch.object(state, "backup", side_effect=error),
        patch.object(state, "resolve", return_value={}),
    ):
        expect_equal(state.main(), 1)
    expect_equal("1.0 GiB is free" in capsys.readouterr().err, expected=True)


def test_live_copy_takes_consistent_sqlite_copies_and_skips_sidecars(
    tmp_path: Path,
) -> None:
    """A database still open for writing is copied consistently with its WAL."""
    source = tmp_path / "source"
    (source / "nested").mkdir(parents=True)
    database = source / "state.db"
    writer = sqlite3.connect(database)
    writer.execute("PRAGMA journal_mode=WAL")
    writer.execute("PRAGMA wal_autocheckpoint=0")
    writer.execute("CREATE TABLE messages (body TEXT)")
    writer.executemany("INSERT INTO messages VALUES (?)", [("kept",)] * 50)
    writer.commit()
    (source / "nested/notes.md").write_text("working file")
    (source / "alias").symlink_to("nested/notes.md")
    with socket.socket(socket.AF_UNIX) as listener:
        listener.bind(str(source / "gateway.sock"))
        try:
            expect_equal((source / "state.db-wal").stat().st_size > 0, expected=True)
            target = tmp_path / "target"
            state.copy_live(source, target)
        finally:
            writer.close()
    copied = sqlite3.connect(target / "state.db")
    try:
        expect_equal(copied.execute("SELECT count(*) FROM messages").fetchone(), (50,))
    finally:
        copied.close()
    expect_equal((target / "state.db-wal").exists(), expected=False)
    expect_equal((target / "state.db-shm").exists(), expected=False)
    expect_equal((target / "gateway.sock").exists(), expected=False)
    expect_equal((target / "nested/notes.md").read_text(), "working file")
    expect_equal((target / "alias").readlink(), Path("nested/notes.md"))


def test_live_copy_skips_files_removed_during_the_walk(tmp_path: Path) -> None:
    """A temporary file deleted mid-copy cannot fail the nightly checkpoint."""
    source = tmp_path / "source"
    source.mkdir()
    (source / "kept").write_text("kept")
    (source / "transient").write_text("transient")
    original = state.shutil.copy2

    def vanishing(path: Path, destination: Path) -> object:
        if Path(path).name == "transient":
            raise FileNotFoundError(path)
        return original(path, destination)

    with patch.object(state.shutil, "copy2", side_effect=vanishing):
        state.copy_live(source, tmp_path / "target")
    expect_equal(sorted(p.name for p in (tmp_path / "target").iterdir()), ["kept"])


@pytest.mark.parametrize("was_paused", [False, True])
def test_live_backup_after_upgrade_releases_the_final_cutoff(
    config: dict,
    runtime: tuple[MagicMock, MagicMock],
    *,
    was_paused: bool,
) -> None:
    """The post-upgrade checkpoint restores writers and dispatch like before."""
    cluster, _repository = runtime
    replicas = dict.fromkeys(config["writers"], 1)
    pending = {
        "replicas": replicas,
        "complete": True,
        "final": True,
        "was_paused": was_paused,
    }
    cluster.read_record.return_value = pending
    maintenance = state.DATASETS / "multica-uploads/.altiera-maintenance"
    maintenance.write_text("paused\n")
    state.backup(config, final=False)
    cluster.scale.assert_called_once_with(replicas)
    expect_equal(maintenance.exists(), was_paused)
    cluster.record.assert_called_with(
        "agent-state-pending",
        {**pending, "final": False},
    )


def test_failed_live_backup_still_releases_the_final_cutoff(
    config: dict,
    runtime: tuple[MagicMock, MagicMock],
) -> None:
    """A failed post-upgrade checkpoint cannot leave the platform fenced."""
    cluster, repository = runtime
    replicas = dict.fromkeys(config["writers"], 1)
    cluster.read_record.return_value = {
        "replicas": replicas,
        "complete": True,
        "final": True,
        "was_paused": False,
    }
    repository.checkpoint.side_effect = RuntimeError("upload failed")
    maintenance = state.DATASETS / "multica-uploads/.altiera-maintenance"
    maintenance.write_text("paused\n")
    with pytest.raises(RuntimeError, match="upload failed"):
        state.backup(config, final=False)
    cluster.scale.assert_called_once_with(replicas)
    expect_equal(maintenance.exists(), expected=False)


def test_retention_runs_before_and_after_the_new_snapshot(
    config: dict,
    runtime: tuple[MagicMock, MagicMock],
) -> None:
    """Old snapshots go first; the latest complete one is protected throughout."""
    _cluster, repository = runtime
    repository.pointer.return_value = {"snapshot": "b" * 64}
    state.backup(config, final=False)
    expect_equal(
        [entry for entry in repository.mock_calls if entry[0] != "pointer"],
        [
            call.retain("b" * 64),
            call.checkpoint(ANY, ANY, ANY),
            call.retain("a" * 64),
        ],
    )


def test_failed_readback_resumes_writers_without_completion(
    config: dict,
    runtime: tuple[MagicMock, MagicMock],
) -> None:
    """A failed final checkpoint cannot authorize teardown or strand ordinary work."""
    cluster, repository = runtime
    repository.checkpoint.side_effect = RuntimeError("readback failed")
    with pytest.raises(RuntimeError, match="readback failed"):
        state.backup(config, final=True)
    expect_equal(cluster.scale.call_args.args[0], dict.fromkeys(config["writers"], 1))
    expect_equal(
        (state.DATASETS / "multica-uploads/.altiera-maintenance").exists(),
        expected=False,
    )
    expect_equal(
        any(
            call.args[0] == "agent-state-checkpoint"
            for call in cluster.record.call_args_list
        ),
        expected=False,
    )


def test_partial_restore_preserves_matching_files_and_rejects_foreign(
    config: dict,
    runtime: tuple[MagicMock, MagicMock],
    tmp_path: Path,
) -> None:
    """A file-copy retry requires matching existing snapshot bytes."""
    del runtime
    payload = tmp_path / "payload"
    manifest = {"schema": 1, "datasets": {}}
    for name in config["datasets"]:
        source = payload / "datasets" / name
        source.mkdir(parents=True)
        (source / "settings").write_text("preserved live state")
        (source / "new").write_text("restored")
        manifest["datasets"][name] = state.inventory(source)
    state.copy_restored_files(config, payload, manifest)
    (state.DATASETS / "hermes-home/settings").write_text("foreign change")
    with pytest.raises(RuntimeError, match="foreign"):
        state.copy_restored_files(config, payload, manifest)


def test_database_wait_retries_until_the_pod_network_is_ready() -> None:
    """A new Job Pod's first seconds without network access cannot fail a restore."""
    failure = RuntimeError("no response")
    with (
        patch.object(state, "run", side_effect=[failure, failure, b""]) as command,
        patch.object(state.time, "sleep") as sleep,
    ):
        state.wait_for_database()
    expect_equal(command.call_count, 3)
    expect_equal(sleep.call_count, 2)


def test_database_wait_has_a_deadline() -> None:
    """An unavailable database still fails the operation within its bound."""
    with (
        patch.object(state, "run", side_effect=RuntimeError("no response")),
        patch.object(state.time, "monotonic", side_effect=[0, 1, 200]),
        patch.object(state.time, "sleep"),
        pytest.raises(TimeoutError, match="did not accept"),
    ):
        state.wait_for_database()
