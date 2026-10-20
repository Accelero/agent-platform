"""Deployment-owned execution environments with native Hermes image selection."""
from __future__ import annotations

import hashlib
import json
import logging
import sqlite3
import threading
import time
import uuid
from contextlib import closing
from datetime import datetime, time as wall_time
from zoneinfo import ZoneInfo

MODES = {"per_session", "per_profile_on_demand", "per_profile_always_running"}
log = logging.getLogger(__name__)


class Conflict(Exception):
    pass


class Forbidden(Exception):
    pass


class Manager:
    def __init__(self, config, backend, database, *, clock=time.time):
        self.config, self.backend, self.clock = config, backend, clock
        self.database = database
        self.lock = threading.RLock()
        self.db = sqlite3.connect(database, check_same_thread=False)
        self.db.row_factory = sqlite3.Row
        self.db.execute("PRAGMA journal_mode=WAL")
        self.db.executescript("""
          CREATE TABLE IF NOT EXISTS environments (
            id TEXT PRIMARY KEY, identity TEXT UNIQUE, deployment TEXT, profile TEXT,
            session TEXT, template TEXT, mode TEXT, fingerprint TEXT, generation TEXT,
            resource TEXT, endpoint TEXT, last_used REAL, idle_since REAL,
            runtime TEXT NOT NULL DEFAULT '{}', image TEXT,
            stale INTEGER DEFAULT 0, closing INTEGER DEFAULT 0, last_schedule TEXT DEFAULT '');
          CREATE TABLE IF NOT EXISTS leases (
            id TEXT PRIMARY KEY, environment TEXT, request TEXT UNIQUE, expires REAL);
        """)
        self.db.commit()
        self.lease_ttl = int(config.get("lease_ttl_seconds", 120))
        if self.lease_ttl < 30:
            raise ValueError("lease_ttl_seconds must be >= 30")

    def _row(self, environment):
        return self.db.execute("SELECT * FROM environments WHERE id=?", (environment,)).fetchone()

    def _count(self, environment):
        return self.db.execute("SELECT count(*) FROM leases WHERE environment=?", (environment,)).fetchone()[0]

    def _expire(self, now):
        # Preserve live leases across restarts. A lost owner drains the
        # generation; retire orphaned commands once remaining owners finish.
        expired = list(self.db.execute("SELECT DISTINCT environment FROM leases WHERE expires<=?", (now,)))
        self.db.execute("DELETE FROM leases WHERE expires<=?", (now,))
        for row in expired:
            self.db.execute("UPDATE environments SET stale=1 WHERE id=?", (row[0],))
            if not self._count(row[0]):
                self.db.execute("UPDATE environments SET idle_since=? WHERE id=?", (now, row[0]))

    def _template(self, deployment, profile, name, mode, image=None):
        if deployment not in self.config.get('deployments', {}):
            raise ValueError("Unknown deployment")
        if name not in self.config.get('templates', {}) or mode not in MODES:
            raise ValueError("Unknown terminal template or lifetime mode")
        template = dict(self.config["templates"][name])
        if image is not None:
            if not isinstance(image, str) or not image or len(image) > 512 or any(c.isspace() or ord(c) < 32 or ord(c) == 127 for c in image):
                raise ValueError("Invalid terminal image")
            template['image'] = image
        fingerprint = hashlib.sha256(json.dumps(template, sort_keys=True).encode()).hexdigest()
        return template, fingerprint

    def _start(self, row, template):
        if template.get('runtime_guest_root'):
            from codex_policy import runtime_template
            template = runtime_template(template, json.loads(row['runtime']), row['session'])
        elif template.get('workdir_scope'):
            from hermes_policy import workdir_template
            template = workdir_template(template, json.loads(row['runtime']), row['mode'], row['profile'])
        generation = row["generation"] or uuid.uuid4().hex[:12]
        resource = json.loads(row["resource"]) if row["resource"] else None
        # Backend create is idempotent by generation name; reconciles a crash between create/save.
        self.db.commit()
        resource, endpoint = self.backend.ensure(row["id"], generation, template, resource)
        self.db.execute("UPDATE environments SET generation=?,resource=?,endpoint=? WHERE id=?",
                        (generation, json.dumps(resource), json.dumps(endpoint) if endpoint else None, row["id"]))
        self.db.commit()
        if not endpoint:
            raise Conflict('Execution pod is provisioning; retry acquisition')
        return self._row(row["id"])

    def _remove_resource(self, row):
        resource = json.loads(row["resource"]) if row["resource"] else {
            "name": f"hermes-exec-{row['id']}-{row['generation']}"}
        # Persist retirement intent before the API request. A later acquisition
        # cannot recreate this generation if its Pod disappears between ticks.
        self.db.execute('UPDATE environments SET stale=1 WHERE id=?', (row['id'],))
        self.db.commit()
        if not self.backend.remove(resource):
            raise Conflict('Execution pod is terminating; retry after deletion is acknowledged')
        self.db.execute("UPDATE environments SET resource=NULL,endpoint=NULL,stale=0,generation=? WHERE id=?",
                        (uuid.uuid4().hex[:12], row["id"]))

    def _delete(self, row):
        self._remove_resource(row)
        self.db.execute("DELETE FROM environments WHERE id=?", (row["id"],))

    def acquire(self, payload):
        now = self.clock()
        deployment, profile, session, name, mode, request = (
            payload.get(k, "") for k in ("deployment", "profile", "session", "template", "mode", "request"))
        if mode not in MODES or not all(isinstance(v, str) and 0 < len(v) <= 512 for v in
                                        (deployment, profile, session, name, request)):
            raise ValueError("Invalid lease request")
        with self.lock:
            try:
                now = self.clock()
                self._expire(now)
                template, fingerprint = self._template(deployment, profile, name, mode, payload.get('image'))
                runtime = payload.get('runtime', {})
                if template.get('runtime_guest_root'):
                    if mode != 'per_session' or not isinstance(runtime, dict):
                        raise ValueError('Invalid Codex execution scope')
                    from codex_policy import runtime_template
                    runtime_template(template, runtime, session)
                elif template.get('workdir_scope'):
                    from hermes_policy import workdir_template
                    workdir_template(template, runtime, mode, profile)
                elif runtime:
                    raise ValueError('Runtime mounts are not permitted for this template')
                identity_parts = [deployment, profile, session if mode == "per_session" else "profile"]
                if template.get('workdir_scope') and 'hermes_home' in runtime:
                    # Different invocation homes/workspaces can coexist, even if
                    # Hermes reuses a session id. Preserve the ordinary profile
                    # identity across upgrades from controllers without metadata.
                    ordinary = {'hermes_home': '/opt/data/profiles/' + profile,
                                'workdir': '/workspace'}
                    if runtime != ordinary:
                        identity_parts.append(runtime)
                identity = json.dumps(identity_parts, sort_keys=True)
                row = self.db.execute("SELECT * FROM environments WHERE identity=?", (identity,)).fetchone()
                if row and mode != "per_session":
                    day = self._scheduled(row, template, now)
                    if day:
                        self.db.execute("UPDATE environments SET stale=1,last_schedule=? WHERE id=?", (day, row["id"]))
                        row = self._row(row["id"])
                existing = self.db.execute("SELECT * FROM leases WHERE request=?", (request,)).fetchone()
                if existing and (not row or existing["environment"] != row["id"]):
                    raise ValueError("Request id belongs to another environment")
                parent = self.db.execute("SELECT * FROM leases WHERE id=?", (payload.get("parent_lease", ""),)).fetchone()
                joined = bool(row and parent and parent["environment"] == row["id"])
                if payload.get("parent_lease") and not (joined or existing):
                    raise ValueError("Parent lease does not own this environment")
                if template.get('serialize_profile'):
                    others = self.db.execute(
                        'SELECT * FROM environments WHERE deployment=? AND profile=?',
                        (deployment, profile)).fetchall()
                    for other in others:
                        if row and other['id'] == row['id']:
                            continue
                        if self._count(other['id']):
                            raise Conflict('The selected profile is already executing')
                        self._delete(other)
                if row and template.get('exclusive') and self._count(row['id']) and not existing:
                    raise Conflict('Codex environment is already executing a run')
                if row and row['template'] != name:
                    if existing or joined or self._count(row['id']):
                        raise Conflict('Terminal template changed while a run is active')
                    self._delete(row)
                    row = None
                if row and row['image'] != payload.get('image'):
                    if existing or joined or self._count(row['id']):
                        raise Conflict('Terminal image changed while a run is active')
                    self._delete(row)
                    row = None
                if row and json.loads(row['runtime']) != runtime:
                    if self._count(row['id']):
                        raise Conflict('Runtime mounts changed while a run is active')
                    self._delete(row)
                    row = None
                if row and not (existing or joined) and (row["closing"] or row["stale"] or row["fingerprint"] != fingerprint or row["mode"] != mode):
                    if self._count(row["id"]):
                        raise Conflict("Environment is draining; retry when its current leases finish")
                    self._delete(row)
                    row = None
                if not row:
                    environment = hashlib.sha256(identity.encode()).hexdigest()[:24]
                    self.db.execute("""INSERT INTO environments
                      (id,identity,deployment,profile,session,template,mode,fingerprint,generation,last_used,idle_since)
                      VALUES (?,?,?,?,?,?,?,?,?,?,?)""", (environment, identity, deployment, profile,
                        session if mode == "per_session" else "", name, mode, fingerprint, uuid.uuid4().hex[:12], 0, now))
                    row = self._row(environment)
                    self.db.execute('UPDATE environments SET runtime=?,image=? WHERE id=?',
                                    (json.dumps(runtime, sort_keys=True), payload.get('image'), environment))
                    row = self._row(environment)
                # A freshly created environment has already satisfied today's recycle boundary.
                day = self._scheduled(row, template, now)
                if day and not row["resource"]:
                    self.db.execute("UPDATE environments SET last_schedule=? WHERE id=?", (day, row["id"]))
                if existing and existing["environment"] != row["id"]:
                    raise ValueError("Request id belongs to another environment")
                # Persist the generation before a backend call, so create can be recovered.
                self.db.commit()
                row = self._start(row, template)
                # A pending Pod owns no lease. Start the TTL only when its ready
                # endpoint is granted, not when the first retry arrived.
                now = self.clock()
                lease = existing["id"] if existing else uuid.uuid4().hex
                self.db.execute("INSERT OR REPLACE INTO leases VALUES (?,?,?,?)",
                                (lease, row["id"], request, now + self.lease_ttl))
                self.db.execute("UPDATE environments SET last_used=?,idle_since=NULL WHERE id=?", (now, row["id"]))
                self.db.commit()
                return {"environment": row["id"], "generation": row["generation"], "lease": lease,
                        "ttl": self.lease_ttl, "ssh": json.loads(row["endpoint"])}
            except Exception:
                self.db.commit()  # Keep reconciliable provisioning state even when startup failed.
                raise

    def heartbeat(self, lease, *, deployment=None):
        # Native API calls are bounded but can wait.
        # Heartbeats use their own DB connection, independent of that lock.
        with closing(sqlite3.connect(self.database, timeout=20)) as db:
            # Serialize the ownership check and renewal with expiry/release.
            db.execute('BEGIN IMMEDIATE')
            now = self.clock()
            row = db.execute("""SELECT l.environment, e.deployment FROM leases l
                JOIN environments e ON e.id=l.environment WHERE l.id=? AND l.expires>?""", (lease, now)).fetchone()
            if not row:
                raise Conflict("Lease expired; stop using this environment")
            if deployment is not None and row[1] != deployment:
                raise Forbidden('Lease belongs to another client')
            db.execute("UPDATE leases SET expires=? WHERE id=?", (now + self.lease_ttl, lease))
            db.execute("UPDATE environments SET last_used=? WHERE id=?", (now, row[0]))
            db.commit()
            return {"ttl": self.lease_ttl}

    def release(self, lease, *, deployment=None, reconcile=True):
        with self.lock:
            row = self.db.execute("SELECT * FROM leases WHERE id=?", (lease,)).fetchone()
            if row:
                if deployment is not None and self._row(row['environment'])['deployment'] != deployment:
                    raise Forbidden('Lease belongs to another client')
                self.db.execute("DELETE FROM leases WHERE id=?", (lease,))
                if not self._count(row["environment"]):
                    self.db.execute("UPDATE environments SET idle_since=? WHERE id=?", (self.clock(), row["environment"]))
            self.db.commit()
        if reconcile:
            self.maintain()
        return {"released": True}

    def close_session(self, deployment, profile, session, *, runtime=None, reconcile=True):
        with self.lock:
            query = "UPDATE environments SET closing=1 WHERE deployment=? AND profile=? AND session=? AND mode='per_session'"
            arguments = [deployment, profile, session]
            if runtime is not None:
                if not isinstance(runtime, dict):
                    raise ValueError('Invalid invocation scope')
                query += ' AND runtime=?'
                arguments.append(json.dumps(runtime, sort_keys=True))
            self.db.execute(query, arguments)
            self.db.commit()
        if reconcile:
            self.maintain()
        return {"closed": True}

    def _scheduled(self, row, template, now):
        schedule = template.get("recycle", {})
        if not schedule.get("at"):
            return ""
        local = datetime.fromtimestamp(now, ZoneInfo(schedule.get("timezone", "Europe/Berlin")))
        hour, minute = map(int, schedule["at"].split(":"))
        # Persist the calendar date to survive restarts and avoid DST/repeated-hour double runs.
        day = local.date().isoformat()
        if local.time() >= wall_time(hour, minute) and row["last_schedule"] != day:
            return day
        return ""

    def maintain(self):
        with self.lock:
            now = self.clock()
            self._expire(now)
            environments = [row['id'] for row in self.db.execute('SELECT id FROM environments')]
            self.db.commit()
        # Each native operation has its own short critical section. Ten worker
        # retirements must not reserve the lock for an entire maintenance batch.
        for environment in environments:
            with self.lock:
                original = self._row(environment)
                if original is None:
                    continue
                try:
                    row = original
                    try:
                        template, fingerprint = self._template(row["deployment"], row["profile"], row["template"], row["mode"], row["image"])
                    except (ValueError, KeyError, Forbidden):
                        if not self._count(row["id"]):
                            self._delete(row)
                        continue
                    day = self._scheduled(row, template, now) if row["mode"] != "per_session" else ""
                    if day:
                        self.db.execute("UPDATE environments SET stale=1,last_schedule=? WHERE id=?", (day, row["id"]))
                        row = self._row(row["id"])
                    if fingerprint != row["fingerprint"]:
                        self.db.execute("UPDATE environments SET stale=1 WHERE id=?", (row["id"],))
                        row = self._row(row["id"])
                    if self._count(row["id"]):
                        continue
                    idle = now - (row["idle_since"] if row["idle_since"] is not None else now)
                    recent = now - row["last_used"] < template.get("recycle", {}).get("inactive_after_seconds", 86400)
                    if row["closing"] or (row["mode"] == "per_session" and idle >= template.get("session_idle_seconds", 86400)):
                        self._delete(row)
                    elif row["stale"]:
                        self._remove_resource(row)
                        self.db.execute("UPDATE environments SET stale=0,fingerprint=? WHERE id=?", (fingerprint, row["id"]))
                        row = self._row(row["id"])
                        # An unused/deleted profile is retired, never resurrected by the timer.
                        if row["mode"] == "per_profile_always_running" and recent:
                            self._start(row, template)
                    elif row["mode"] == "per_profile_on_demand" and idle >= template.get("on_demand_idle_seconds", 60) and row["resource"]:
                        self._remove_resource(row)
                    elif row['mode'] == 'per_profile_always_running' and recent:
                        self.db.commit()
                        if not row['endpoint']:
                            # A pending Pod is retried in place, including after
                            # a manager restart. Readiness is not a crash.
                            self._start(row, template)
                        elif not self.backend.is_running(json.loads(row['resource'])):
                            self._remove_resource(row)
                            self._start(self._row(row['id']), template)
                except Conflict:
                    continue  # Pending readiness/termination; reconcile on the next tick.
                except Exception:
                    log.exception("Environment maintenance failed for %s", original["id"])
                finally:
                    self.db.commit()

    def status(self, *, deployment=None):
        with self.lock:
            return [{"id": row["id"], "deployment": row["deployment"], "profile": row["profile"],
                     "session": row["session"],
                     "mode": row["mode"], "generation": row["generation"], "stale": bool(row["stale"]),
                     "provisioned": bool(row["resource"]), "leases": self._count(row["id"]),
                     "last_used": row["last_used"]} for row in self.db.execute(
                         "SELECT * FROM environments WHERE (? IS NULL OR deployment=?)", (deployment, deployment))]
