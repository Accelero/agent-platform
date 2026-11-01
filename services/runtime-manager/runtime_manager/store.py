"""SQLite state: environments and leases.

The connection lock is held only for database work, never across a cluster
call, so heartbeats and releases do not wait for `kubectl`. Every method is one
transaction; `transaction()` groups several into one.
"""
from __future__ import annotations

import enum
import json
import sqlite3
import threading
from contextlib import contextmanager

from .domain.errors import Forbidden, Invalid, LeaseExpired, LeaseNotLive
from .domain.model import (Environment, Failure, Lease, LeaseState, Mode, Retire)

SCHEMA_VERSION = 1


def _values(kind: type[enum.Enum]) -> str:
    return ','.join(f"'{member.value}'" for member in kind)


SCHEMA = (
    f"""CREATE TABLE environments (
      id TEXT PRIMARY KEY, identity TEXT NOT NULL UNIQUE,
      deployment TEXT NOT NULL, profile TEXT NOT NULL, session TEXT NOT NULL,
      template TEXT NOT NULL, mode TEXT NOT NULL CHECK (mode IN ({_values(Mode)})),
      image TEXT, runtime TEXT NOT NULL DEFAULT '{{}}',
      fingerprint TEXT NOT NULL, generation TEXT NOT NULL, endpoint TEXT,
      started REAL, last_used REAL NOT NULL DEFAULT 0, idle_since REAL,
      failure TEXT CHECK (failure IS NULL OR failure IN ({_values(Failure)})),
      failed_at REAL,
      retire TEXT CHECK (retire IS NULL OR retire IN ({_values(Retire)})),
      draining_since REAL, last_schedule TEXT NOT NULL DEFAULT '')""",
    f"""CREATE TABLE leases (
      id TEXT PRIMARY KEY, environment TEXT NOT NULL, request TEXT NOT NULL UNIQUE,
      state TEXT NOT NULL CHECK (state IN ({_values(LeaseState)})),
      created REAL NOT NULL, expires REAL NOT NULL, ended REAL)""",
    'CREATE INDEX leases_environment ON leases (environment, state)',
)

LEGACY_LEASE_TTL = 120


def _environment(row: sqlite3.Row) -> Environment:
    return Environment(
        id=row['id'], identity=row['identity'], deployment=row['deployment'],
        profile=row['profile'], session=row['session'], template=row['template'],
        mode=Mode(row['mode']), image=row['image'], runtime=json.loads(row['runtime']),
        fingerprint=row['fingerprint'], generation=row['generation'],
        endpoint=json.loads(row['endpoint']) if row['endpoint'] else None,
        started=row['started'], last_used=row['last_used'], idle_since=row['idle_since'],
        failure=Failure(row['failure']) if row['failure'] else None,
        failed_at=row['failed_at'],
        retire=Retire(row['retire']) if row['retire'] else None,
        draining_since=row['draining_since'], last_schedule=row['last_schedule'])


def _lease(row: sqlite3.Row) -> Lease:
    return Lease(id=row['id'], environment=row['environment'], request=row['request'],
                 state=LeaseState(row['state']), created=row['created'],
                 expires=row['expires'])


class Store:
    def __init__(self, path: str):
        self._db = sqlite3.connect(path, check_same_thread=False, isolation_level=None,
                                   timeout=20)
        self._db.row_factory = sqlite3.Row
        self._db.execute('PRAGMA journal_mode=WAL')
        self._lock = threading.RLock()
        self._depth = 0
        self._upgrade()

    def close(self) -> None:
        with self._lock:
            self._db.close()

    @contextmanager
    def transaction(self):
        with self._lock:
            if self._depth:
                self._depth += 1
                try:
                    yield
                finally:
                    self._depth -= 1
                return
            self._db.execute('BEGIN IMMEDIATE')
            self._depth = 1
            try:
                yield
            except BaseException:
                self._db.execute('ROLLBACK')
                raise
            else:
                self._db.execute('COMMIT')
            finally:
                self._depth = 0

    def _one(self, sql: str, *args):
        with self.transaction():
            return self._db.execute(sql, args).fetchone()

    def _all(self, sql: str, *args):
        with self.transaction():
            return self._db.execute(sql, args).fetchall()

    def _run(self, sql: str, *args) -> int:
        with self.transaction():
            return self._db.execute(sql, args).rowcount

    # Schema --------------------------------------------------------------------

    def _upgrade(self) -> None:
        with self.transaction():
            version = self._db.execute('PRAGMA user_version').fetchone()[0]
            if version > SCHEMA_VERSION:
                raise RuntimeError('The database was written by a newer manager')
            if version == SCHEMA_VERSION:
                return
            tables = {row[0] for row in self._db.execute(
                "SELECT name FROM sqlite_master WHERE type='table'")}
            if 'environments' in tables:
                self._db.execute('ALTER TABLE environments RENAME TO environments_v0')
                self._db.execute('ALTER TABLE leases RENAME TO leases_v0')
            for statement in SCHEMA:
                self._db.execute(statement)
            if 'environments' in tables:
                self._convert_v0()
            self._db.execute(f'PRAGMA user_version = {SCHEMA_VERSION}')

    def _convert_v0(self) -> None:
        """Adopt the previous manager's rows: its Pods keep their names."""
        have = {row[1] for row in self._db.execute('PRAGMA table_info(environments_v0)')}

        def column(name: str, default: str) -> str:
            return name if name in have else default

        self._db.execute(f"""INSERT INTO environments
            (id, identity, deployment, profile, session, template, mode, image, runtime,
             fingerprint, generation, endpoint, last_used, idle_since, retire, last_schedule)
            SELECT id, identity, deployment, profile, session, template, mode,
              {column('image', 'NULL')}, COALESCE({column('runtime', 'NULL')}, '{{}}'),
              COALESCE(fingerprint, ''), generation, endpoint, COALESCE(last_used, 0),
              idle_since,
              CASE WHEN COALESCE({column('closing', '0')}, 0) THEN 'closed'
                   WHEN COALESCE({column('stale', '0')}, 0) THEN 'config' END,
              COALESCE({column('last_schedule', "''")}, '')
            FROM environments_v0""")
        self._db.execute(f"""INSERT INTO leases (id, environment, request, state, created, expires)
            SELECT id, environment, request, 'active', expires - {LEGACY_LEASE_TTL}, expires
            FROM leases_v0""")
        self._db.execute('DROP TABLE environments_v0')
        self._db.execute('DROP TABLE leases_v0')

    # Environments --------------------------------------------------------------

    def environment(self, environment: str) -> Environment | None:
        row = self._one('SELECT * FROM environments WHERE id=?', environment)
        return _environment(row) if row else None

    def by_identity(self, identity: str) -> Environment | None:
        row = self._one('SELECT * FROM environments WHERE identity=?', identity)
        return _environment(row) if row else None

    def environments(self, deployment: str | None = None) -> list[Environment]:
        return [_environment(row) for row in self._all(
            'SELECT * FROM environments WHERE (? IS NULL OR deployment=?) ORDER BY id',
            deployment, deployment)]

    def peers(self, deployment: str, profile: str, exclude: str) -> list[Environment]:
        return [_environment(row) for row in self._all(
            'SELECT * FROM environments WHERE deployment=? AND profile=? AND id<>?',
            deployment, profile, exclude)]

    def pod_names(self) -> set[str]:
        return {f'hermes-exec-{row[0]}-{row[1]}' for row in self._all(
            'SELECT id, generation FROM environments')}

    def insert(self, env: Environment, now: float) -> None:
        self._run(
            """INSERT INTO environments (id, identity, deployment, profile, session, template,
               mode, image, runtime, fingerprint, generation, last_used, idle_since)
               VALUES (?,?,?,?,?,?,?,?,?,?,?,0,?)""",
            env.id, env.identity, env.deployment, env.profile, env.session, env.template,
            env.mode.value, env.image, json.dumps(env.runtime, sort_keys=True),
            env.fingerprint, env.generation, now)

    def retarget(self, environment: str, *, template: str, image: str | None,
                 runtime: dict, mode: Mode, fingerprint: str) -> None:
        """Point the record at a new configuration; the planner replaces the Pod."""
        self._run(
            """UPDATE environments SET template=?, image=?, runtime=?, mode=?, fingerprint=?,
               retire='config', failure=NULL, failed_at=NULL, draining_since=NULL WHERE id=?""",
            template, image, json.dumps(runtime, sort_keys=True), mode.value, fingerprint,
            environment)

    def set_retire(self, environment: str, reason: Retire, day: str = '') -> None:
        self._run('UPDATE environments SET retire=?, last_schedule=CASE WHEN ?<>\'\' THEN ? '
                  'ELSE last_schedule END WHERE id=?', reason.value, day, day, environment)

    def close_sessions(self, deployment: str, profile: str, session: str,
                       runtime: dict | None) -> list[str]:
        """Mark a session's environments closed; they go with their Pod."""
        sql = ("SELECT id FROM environments WHERE deployment=? AND profile=? AND session=? "
               "AND mode='per_session'")
        args = [deployment, profile, session]
        if runtime is not None:
            sql += ' AND runtime=?'
            args.append(json.dumps(runtime, sort_keys=True))
        with self.transaction():
            ids = [row[0] for row in self._db.execute(sql, args)]
            for environment in ids:
                self.set_retire(environment, Retire.CLOSED)
        return ids

    def start_draining(self, environment: str, now: float) -> None:
        self._run('UPDATE environments SET draining_since=? WHERE id=? '
                  'AND draining_since IS NULL', now, environment)

    def mark_created(self, environment: str, now: float, day: str) -> None:
        self._run('UPDATE environments SET started=?, last_schedule=CASE WHEN ?<>\'\' '
                  'THEN ? ELSE last_schedule END WHERE id=?', now, day, day, environment)

    def mark_started(self, environment: str, now: float) -> None:
        self._run('UPDATE environments SET started=? WHERE id=?', now, environment)

    def mark_ready(self, environment: str, endpoint: dict, now: float) -> None:
        """Record readiness; idle time counts from now unless leases are live."""
        with self.transaction():
            self._run('UPDATE environments SET endpoint=?, idle_since=CASE WHEN '
                      '(SELECT count(*) FROM leases WHERE environment=? AND state=\'active\' '
                      'AND expires>?)=0 THEN ? ELSE idle_since END WHERE id=?',
                      json.dumps(endpoint), environment, now, now, environment)

    def fail(self, environment: str, failure: Failure, now: float) -> None:
        self._run('UPDATE environments SET failure=?, failed_at=? WHERE id=?',
                  failure.value, now, environment)

    def replace(self, environment: str, generation: str, fingerprint: str,
                now: float) -> None:
        """A fresh generation: the previous Pod is confirmed absent."""
        self._run(
            """UPDATE environments SET generation=?, fingerprint=?, endpoint=NULL, started=NULL,
               retire=NULL, draining_since=NULL, failure=NULL, failed_at=NULL, idle_since=?
               WHERE id=?""", generation, fingerprint, now, environment)

    def forget(self, environment: str, now: float) -> None:
        with self.transaction():
            self.end_leases(environment, now, LeaseState.REVOKED)
            self._run('DELETE FROM environments WHERE id=?', environment)

    # Leases --------------------------------------------------------------------

    def lease(self, lease: str) -> Lease | None:
        row = self._one('SELECT * FROM leases WHERE id=?', lease)
        return _lease(row) if row else None

    def lease_by_request(self, request: str) -> Lease | None:
        row = self._one('SELECT * FROM leases WHERE request=?', request)
        return _lease(row) if row else None

    def live(self, environment: str, now: float) -> int:
        return self._one("SELECT count(*) FROM leases WHERE environment=? AND state='active' "
                         "AND expires>?", environment, now)[0]

    def grant(self, lease: str, environment: str, request: str, now: float, ttl: int,
              max_lease: int) -> str:
        """Create the lease of a request, or renew its live one (never past the cap)."""
        with self.transaction():
            current = self.lease_by_request(request)
            if current is not None:
                if (current.state is not LeaseState.ACTIVE or current.expires <= now
                        or current.created + max_lease <= now):
                    raise LeaseNotLive('The lease of this request has ended')
                if current.environment != environment:
                    raise Invalid('Request id belongs to another environment')
                lease = current.id
                self._run('UPDATE leases SET expires=min(?, created+?) WHERE id=?',
                          now + ttl, max_lease, lease)
            else:
                self._run("INSERT INTO leases (id, environment, request, state, created, expires) "
                          "VALUES (?,?,?,'active',?,?)", lease, environment, request, now,
                          now + min(ttl, max_lease))
            self._run('UPDATE environments SET last_used=?, idle_since=NULL WHERE id=?',
                      now, environment)
        return lease

    def heartbeat(self, lease: str, deployment: str | None, now: float, ttl: int,
                  max_lease: int) -> None:
        with self.transaction():
            row = self._one(
                'SELECT l.*, e.deployment AS owner FROM leases l '
                'LEFT JOIN environments e ON e.id=l.environment WHERE l.id=?', lease)
            if (row is None or row['owner'] is None or row['state'] != 'active'
                    or row['expires'] <= now or row['created'] + max_lease <= now):
                raise LeaseExpired('Lease expired; stop using this environment')
            if deployment is not None and row['owner'] != deployment:
                raise Forbidden('Lease belongs to another client')
            self._run('UPDATE leases SET expires=min(?, created+?) WHERE id=?',
                      now + ttl, max_lease, lease)
            self._run('UPDATE environments SET last_used=? WHERE id=?', now, row['environment'])

    def release(self, lease: str, deployment: str | None, now: float) -> None:
        with self.transaction():
            row = self._one(
                'SELECT l.*, e.deployment AS owner FROM leases l '
                'LEFT JOIN environments e ON e.id=l.environment WHERE l.id=?', lease)
            if row is None:
                return
            if deployment is not None and row['owner'] not in (None, deployment):
                raise Forbidden('Lease belongs to another client')
            if row['state'] == 'active':
                self._run("UPDATE leases SET state='released', ended=? WHERE id=?", now, lease)
                self.idle_if_unleased(row['environment'], now)

    def expire(self, now: float) -> list[str]:
        """End leases that were not renewed; the environments they belonged to."""
        with self.transaction():
            rows = self._db.execute("SELECT id, environment, expires FROM leases WHERE "
                                    "state='active' AND expires<=?", (now,)).fetchall()
            for row in rows:
                self._db.execute("UPDATE leases SET state='expired', ended=? WHERE id=?",
                                 (row['expires'], row['id']))
            return sorted({row['environment'] for row in rows})

    def lost(self, environment: str, now: float) -> None:
        """An owner stopped renewing: the generation drains, idle time restarts."""
        with self.transaction():
            self._run("UPDATE environments SET retire='lease_lost' WHERE id=? AND retire IS NULL",
                      environment)
            self.idle_if_unleased(environment, now)

    def idle_if_unleased(self, environment: str, now: float) -> None:
        self._run("UPDATE environments SET idle_since=? WHERE id=? AND NOT EXISTS "
                  "(SELECT 1 FROM leases WHERE environment=? AND state='active' AND expires>?)",
                  now, environment, environment, now)

    def end_leases(self, environment: str, now: float, state: LeaseState) -> int:
        return self._run("UPDATE leases SET state=?, ended=? WHERE environment=? "
                         "AND state='active'", state.value, now, environment)

    def prune(self, now: float, memory: int) -> None:
        self._run("DELETE FROM leases WHERE state<>'active' AND ended<=?", now - memory)
