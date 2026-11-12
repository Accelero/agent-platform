"""The database of the previous manager is adopted in place, with its workers (X12)."""

import hashlib
import json
import sqlite3

import pytest
from runtime_manager.cluster import render_pod
from runtime_manager.domain.model import Retire
from runtime_manager.service import environment_id
from runtime_manager.store import SCHEMA_VERSION, Store

from tests.support.world import DEFAULT_IMAGE, default_config, make_world

LEGACY_SCHEMA = """
CREATE TABLE environments (
  id TEXT PRIMARY KEY, identity TEXT UNIQUE, deployment TEXT, profile TEXT,
  session TEXT, template TEXT, mode TEXT, fingerprint TEXT, generation TEXT,
  resource TEXT, endpoint TEXT, last_used REAL, idle_since REAL,
  runtime TEXT NOT NULL DEFAULT '{}', image TEXT,
  stale INTEGER DEFAULT 0, closing INTEGER DEFAULT 0, last_schedule TEXT DEFAULT '');
CREATE TABLE leases (
  id TEXT PRIMARY KEY, environment TEXT, request TEXT UNIQUE, expires REAL);
"""
ENDPOINT = json.dumps({"host": "10.42.0.9", "port": 2222, "user": "worker"})


def identity(profile, session="profile"):
    return json.dumps(["fixture", profile, session], sort_keys=True)


def legacy_database(path, now, fingerprint):
    """What manager 0.6.9 leaves behind: a leased worker, a stale idle one, a
    closing session.
    """
    db = sqlite3.connect(path)
    db.executescript(LEGACY_SCHEMA)
    rows = [
        # id, identity, profile, session, mode, generation, resource,
        # endpoint, stale, closing
        (
            environment_id(identity("busy")),
            identity("busy"),
            "busy",
            "",
            "per_profile_on_demand",
            "a" * 12,
            "yes",
            ENDPOINT,
            0,
            0,
        ),
        (
            environment_id(identity("stale")),
            identity("stale"),
            "stale",
            "",
            "per_profile_on_demand",
            "b" * 12,
            None,
            None,
            1,
            0,
        ),
        (
            environment_id(identity("task", "t1")),
            identity("task", "t1"),
            "task",
            "t1",
            "per_session",
            "c" * 12,
            None,
            None,
            0,
            1,
        ),
    ]
    for (
        key,
        ident,
        profile,
        session,
        mode,
        generation,
        resource,
        endpoint,
        stale,
        closing,
    ) in rows:
        db.execute(
            "INSERT INTO environments (id, identity, deployment, profile, session,"
            " template, mode, fingerprint, generation, resource, endpoint,"
            " last_used, idle_since, stale, closing)"
            " VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (
                key,
                ident,
                "fixture",
                profile,
                session,
                "terminal",
                mode,
                fingerprint,
                generation,
                json.dumps({"name": "x"}) if resource else None,
                endpoint,
                now - 5,
                None,
                stale,
                closing,
            ),
        )
    scoped = hashlib.sha256(json.dumps(["fixture", "req-live"]).encode()).hexdigest()
    db.execute(
        "INSERT INTO leases VALUES (?,?,?,?)", ("live", rows[0][0], scoped, now + 90)
    )
    db.commit()
    db.close()
    return rows[0][0]


def test_a_legacy_database_is_converted_and_its_worker_adopted(tmp_path):
    (tmp_path / "probe").mkdir()
    probe = make_world(tmp_path / "probe", config=default_config())
    fingerprint = probe.service.catalog.resolve("fixture", "terminal").fingerprint
    probe.close()
    path = tmp_path / "state.sqlite3"
    now = 1_000_000.0
    busy = legacy_database(path, now, fingerprint)

    world = make_world(tmp_path, config=default_config(), database=path)
    try:
        world.clock.now = now
        # The previous manager's Pod is running; the new manager must keep it.
        name = f"hermes-exec-{busy}-{'a' * 12}"
        world.kube.add(
            render_pod(
                name,
                busy,
                {"image": DEFAULT_IMAGE, "kubernetes": {"role": "hermes"}},
                "hermes-execution",
                [],
            ),
            created=now - 600,
        )
        states = {env.profile: env for env in world.store.environments()}
        assert (
            states["busy"].retire is None
            and states["busy"].endpoint["host"] == "10.42.0.9"
        )
        assert states["stale"].retire is Retire.CONFIG
        assert states["task"].retire is Retire.CLOSED

        world.service.maintain()

        assert (
            world.kube.created() == [] and world.kube.deleted() == []
        )  # the worker is adopted
        by_profile = {env["profile"]: env for env in world.environments()}
        assert (
            by_profile["busy"]["leases"] == 1 and by_profile["busy"]["phase"] == "ready"
        )
        assert (
            by_profile["stale"]["generation"] != "b" * 12
        )  # replaced, no Pod to remove
        assert "task" not in by_profile  # closed session forgotten
        world.post("/v1/leases/heartbeat", lease="live")  # the live lease survived
        assert world.acquire(profile="busy", request="req-live")["lease"] == "live"
    finally:
        world.close()


def test_the_upgrade_runs_once_and_a_newer_database_is_refused(tmp_path):
    path = tmp_path / "state.sqlite3"
    first = Store(str(path))
    assert first._db.execute("PRAGMA user_version").fetchone()[0] == SCHEMA_VERSION
    first.close()
    Store(str(path)).close()  # reopening a current database changes nothing
    db = sqlite3.connect(path)
    db.execute(f"PRAGMA user_version = {SCHEMA_VERSION + 1}")
    db.commit()
    db.close()
    with pytest.raises(RuntimeError, match="newer manager"):
        Store(str(path))


def test_state_constraints_reject_values_outside_the_enums(tmp_path):
    store = Store(str(tmp_path / "state.sqlite3"))
    try:
        for column, bad in (
            ("mode", "forever"),
            ("retire", "whenever"),
            ("failure", "sad"),
        ):
            with pytest.raises(sqlite3.IntegrityError):
                store._db.execute(
                    "INSERT INTO environments (id, identity, deployment, profile,"
                    " session,"
                    " template, mode, fingerprint, generation, " + column + ")"
                    " VALUES ('i','i','d','p','','t','per_session','f','g',?)"
                    if column != "mode"
                    else "INSERT INTO environments (id, identity, deployment, profile,"
                    " session, template, fingerprint, generation, mode)"
                    " VALUES ('i','i','d','p','','t','f','g',?)",
                    (bad,),
                )
    finally:
        store.close()
