"""Use cases: acquire, heartbeat, release, close, maintain, orphans.

Every desired-state change is one store transaction. Cluster calls run outside
any transaction, under the lock of the profile they belong to, so heartbeats,
releases and status reads never wait for `kubectl`. One `_reconcile` serves
both a waiting acquire and the maintenance tick: it observes the Pod, asks the
planner and executes the actions until nothing is left to do.
"""
from __future__ import annotations

import hashlib
import json
import logging
import threading
import time
import uuid

from .config import Catalog, Resolved, mode_of, settings_from
from .domain import planner
from .domain.admission import Facts, Request, Verdict, admit
from .domain.errors import BackendFailed, Draining, Invalid, LeaseNotLive, Pending
from .domain.model import Environment, LeaseState, Mode, Retire, Timers
from .scope import expand
from .store import Store

log = logging.getLogger(__name__)
PASSES = 3
ORDINARY_HOME = '/opt/data/profiles/'


def environment_id(identity: str) -> str:
    return hashlib.sha256(identity.encode()).hexdigest()[:24]


class Service:
    def __init__(self, config: dict, cluster, store: Store, *, clock=time.time):
        self.config, self.cluster, self.store, self.clock = config, cluster, store, clock
        self.settings = settings_from(config)
        self.catalog = Catalog(config)
        self._guard = threading.Lock()
        self._locks: dict[tuple[str, str], threading.RLock] = {}
        self._orphans: dict[str, dict] = {}

    def _lock(self, deployment: str, profile: str) -> threading.RLock:
        with self._guard:
            return self._locks.setdefault((deployment, profile), threading.RLock())

    # Acquire -------------------------------------------------------------------

    def _request(self, payload: dict) -> Request:
        deployment, profile, session, name, mode, request = (
            payload.get(key, '') for key in
            ('deployment', 'profile', 'session', 'template', 'mode', 'request'))
        if not all(isinstance(v, str) and 0 < len(v) <= 512
                   for v in (deployment, profile, session, name, request)):
            raise Invalid('Invalid lease request')
        mode = mode_of(mode)
        resolved = self.catalog.resolve(deployment, name, payload.get('image'))
        runtime = payload.get('runtime', {})
        expand(resolved.template, runtime=runtime, mode=mode, profile=profile, session=session)
        parts = [deployment, profile, session if mode is Mode.PER_SESSION else 'profile']
        if resolved.template.get('workdir_scope') and 'hermes_home' in runtime:
            # Different invocation homes and workspaces coexist even if Hermes
            # reuses a session id. The ordinary profile identity stays unchanged.
            ordinary = {'hermes_home': ORDINARY_HOME + profile, 'workdir': '/workspace'}
            if runtime != ordinary:
                parts.append(runtime)
        identity = json.dumps(parts, sort_keys=True)
        return Request(
            deployment=deployment, profile=profile, session=session, template=name, mode=mode,
            request=request, image=payload.get('image'), runtime=runtime,
            parent_lease=payload.get('parent_lease', ''), fingerprint=resolved.fingerprint,
            identity=identity, environment=environment_id(identity),
            serialize=bool(resolved.template.get('serialize_profile')),
            exclusive=bool(resolved.template.get('exclusive')))

    def acquire(self, payload: dict) -> dict:
        req = self._request(payload)
        with self._lock(req.deployment, req.profile):
            self._expire(self.clock())
            for _ in range(2):
                env, held = self._admit(req)
                env = self._reconcile(env.id, wanted=True)
                if env is not None:
                    return self._outcome(req, env, held)
        raise Pending('Execution environment is being replaced; retry acquisition')

    def _admit(self, req: Request) -> tuple[Environment, bool]:
        now = self.clock()
        row = self.store.by_identity(req.identity)
        existing = self.store.lease_by_request(req.request)
        parent = self.store.lease(req.parent_lease) if req.parent_lease else None
        peers = (self.store.peers(req.deployment, req.profile, req.environment)
                 if req.serialize else [])
        facts = Facts(
            row=row, live=self.store.live(row.id, now) if row else 0, existing=existing,
            parent=parent, peers_busy=any(self.store.live(p.id, now) for p in peers),
            now=now, settings=self.settings)
        verdict = admit(req, facts)
        for peer in peers:  # an idle sibling makes room for this profile worker
            self.store.set_retire(peer.id, Retire.CLOSED)
            if self._reconcile(peer.id, wanted=False) is not None:
                raise Draining('The previous profile worker is still stopping')
        if verdict is Verdict.CREATE:
            self.store.insert(Environment(
                id=req.environment, identity=req.identity, deployment=req.deployment,
                profile=req.profile, session=req.session if req.mode is Mode.PER_SESSION else '',
                template=req.template, mode=req.mode, image=req.image, runtime=req.runtime,
                fingerprint=req.fingerprint, generation=uuid.uuid4().hex[:12]), now)
        elif verdict is Verdict.RETARGET:
            self.store.retarget(row.id, template=req.template, image=req.image,
                                runtime=req.runtime, mode=req.mode, fingerprint=req.fingerprint)
        held = bool(existing is not None or (
            row and parent and parent.state is LeaseState.ACTIVE and parent.environment == row.id))
        return self.store.by_identity(req.identity), held

    def _outcome(self, req: Request, env: Environment, held: bool) -> dict:
        now = self.clock()
        if env.failure is not None:
            raise BackendFailed(env.failure.value)
        existing = self.store.lease_by_request(req.request)
        if existing is not None and existing.state is not LeaseState.ACTIVE:
            raise LeaseNotLive('The lease of this request has ended')
        if env.retire is not None and not held:
            raise (Draining if self.store.live(env.id, now) else Pending)(
                'Execution environment is being replaced; retry acquisition')
        if env.endpoint is None:
            raise Pending('Execution pod is provisioning; retry acquisition')
        lease = self.store.grant(uuid.uuid4().hex, env.id, req.request, now,
                                 self.settings.lease_ttl, self.settings.max_lease)
        return {'environment': env.id, 'generation': env.generation, 'lease': lease,
                'ttl': self.settings.lease_ttl, 'ssh': env.endpoint}

    # Lease operations ----------------------------------------------------------

    def heartbeat(self, lease: str, *, deployment: str | None = None) -> dict:
        self.store.heartbeat(lease, deployment, self.clock(), self.settings.lease_ttl,
                             self.settings.max_lease)
        return {'ttl': self.settings.lease_ttl}

    def release(self, lease: str, *, deployment: str | None = None, reconcile: bool = True) -> dict:
        self.store.release(lease, deployment, self.clock())
        if reconcile:
            self.maintain()
        return {'released': True}

    def close_session(self, deployment: str, profile: str, session: str, *,
                      runtime: dict | None = None, reconcile: bool = True) -> dict:
        if runtime is not None and not isinstance(runtime, dict):
            raise Invalid('Invalid invocation scope')
        self.store.close_sessions(deployment, profile, session, runtime)
        if reconcile:
            self.maintain()
        return {'closed': True}

    def _expire(self, now: float) -> None:
        with self.store.transaction():
            for environment in self.store.expire(now):
                self.store.lost(environment, now)
            self.store.prune(now, self.settings.request_memory)

    # Reconciliation ------------------------------------------------------------

    def maintain(self) -> None:
        self._expire(self.clock())
        for env in self.store.environments():
            with self._lock(env.deployment, env.profile):
                try:
                    self._reconcile(env.id, wanted=False)
                except Exception:
                    log.exception('Environment maintenance failed for %s', env.id)

    def _resolve(self, env: Environment) -> Resolved | None:
        try:
            return self.catalog.resolve(env.deployment, env.template, env.image)
        except ValueError:
            return None

    def _reconcile(self, environment: str, *, wanted: bool) -> Environment | None:
        """Observe, plan and execute until nothing is left to do (at most a few passes)."""
        for _ in range(PASSES):
            env = self.store.environment(environment)
            if env is None:
                return None
            resolved = self._resolve(env)
            if resolved is None:  # its template or deployment is no longer configured
                self.store.set_retire(env.id, Retire.CLOSED)
                env = self.store.environment(env.id)
            now = self.clock()
            actions = planner.plan(
                env, self.cluster.observe(env.pod_name), self.store.live(env.id, now),
                now, self.settings, resolved.timers if resolved else Timers(),
                resolved.fingerprint if resolved else env.fingerprint, wanted)
            if not actions:
                return env
            for action in actions:
                self._execute(action, env, resolved, now)
        return self.store.environment(environment)

    def _execute(self, action, env: Environment, resolved: Resolved | None, now: float) -> None:
        store = self.store
        match action:
            case planner.SetRetire(reason, day):
                store.set_retire(env.id, reason, day)
            case planner.StartDraining():
                store.start_draining(env.id, now)
            case planner.EndLeases(why):
                ended = store.end_leases(env.id, now, LeaseState.REVOKED)
                log.warning('Ended %d lease(s) of environment %s: %s', ended, env.id, why)
            case planner.CreatePod(day):
                template = expand(resolved.template, runtime=env.runtime, mode=env.mode,
                                  profile=env.profile, session=env.session)
                self.cluster.create(env.pod_name, env.id, template)
                store.mark_created(env.id, now, day)
            case planner.DeletePod():
                self.cluster.delete(env.pod_name)
            case planner.MarkStarted():
                store.mark_started(env.id, now)
            case planner.MarkReady(endpoint):
                store.mark_ready(env.id, endpoint, now)
            case planner.Fail(failure):
                store.fail(env.id, failure, now)
                log.warning('Worker of environment %s cannot start: %s', env.id, failure.value)
            case planner.Replace(fingerprint):
                store.replace(env.id, uuid.uuid4().hex[:12], fingerprint, now)
            case planner.Forget():
                store.forget(env.id, now)

    # Reports -------------------------------------------------------------------

    def status(self, *, deployment: str | None = None) -> list[dict]:
        now = self.clock()
        return [{
            'id': env.id, 'deployment': env.deployment, 'profile': env.profile,
            'session': env.session, 'mode': env.mode.value, 'generation': env.generation,
            'stale': env.retire is not None, 'retire': env.retire.value if env.retire else None,
            'failure': env.failure.value if env.failure else None,
            'phase': ('failed' if env.failure else 'retiring' if env.retire
                      else 'ready' if env.endpoint else 'provisioning' if env.started
                      else 'absent'),
            'provisioned': env.started is not None or env.endpoint is not None,
            'leases': self.store.live(env.id, now), 'last_used': env.last_used}
            for env in self.store.environments(deployment)]

    def scan_orphans(self) -> list[dict]:
        """Pods labelled as managed that no record names. Never adopted; deleted
        only under `orphan_policy: delete`, after the grace period."""
        # Records are persisted before their Pods are created. Snapshot names
        # after the cluster list so a concurrent acquire cannot look orphaned.
        pods = self.cluster.list_managed()
        known = self.store.pod_names()
        now = self.clock()
        found: dict[str, dict] = {}
        for pod in pods:
            if pod.name in known or now - pod.created < self.settings.orphan_grace:
                continue
            if pod.name not in self._orphans:
                log.warning('Worker Pod %s belongs to no environment record', pod.name)
            found[pod.name] = {'name': pod.name, 'created': pod.created,
                               'first_seen': self._orphans.get(pod.name, {}).get('first_seen', now)}
            if self.settings.orphan_policy == 'delete':
                self.cluster.delete(pod.name)
        self._orphans = found
        return self.orphans()

    def orphans(self) -> list[dict]:
        return sorted(self._orphans.values(), key=lambda orphan: orphan['name'])
