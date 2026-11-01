"""Acquire admission: may this request use, replace or create an environment?

Pure. The service gathers the facts, asks `admit` and applies the verdict. A
request is refused with a typed error; otherwise the verdict says whether the
environment is created, re-targeted at the request's configuration or reused.
"""
from __future__ import annotations

import enum
from dataclasses import dataclass

from .errors import BackendFailed, Conflict, Draining, Invalid, LeaseNotLive
from .model import Environment, Lease, LeaseState, Mode, Retire, Settings


class Verdict(enum.Enum):
    CREATE = 'create'
    RETARGET = 'retarget'
    REUSE = 'reuse'


@dataclass(frozen=True, slots=True)
class Request:
    """A validated acquire request. `request` is the client-scoped idempotency key."""
    deployment: str
    profile: str
    session: str
    template: str
    mode: Mode
    request: str
    image: str | None
    runtime: dict
    parent_lease: str
    fingerprint: str
    identity: str = ''
    environment: str = ''
    serialize: bool = False
    exclusive: bool = False


@dataclass(frozen=True, slots=True)
class Facts:
    """What the service knows about the world when it asks."""
    row: Environment | None
    live: int
    existing: Lease | None
    parent: Lease | None
    peers_busy: bool
    now: float
    settings: Settings


def admit(req: Request, facts: Facts) -> Verdict:
    row, existing = facts.row, facts.existing
    if existing is not None and existing.state is not LeaseState.ACTIVE:
        raise LeaseNotLive('The lease of this request has ended')
    if existing is not None and (row is None or existing.environment != row.id):
        raise Invalid('Request id belongs to another environment')
    parent = facts.parent
    joined = bool(row and parent and parent.state is LeaseState.ACTIVE
                  and parent.environment == row.id)
    if req.parent_lease and not (joined or existing):
        raise Invalid('Parent lease does not own this environment')
    if req.serialize and facts.peers_busy:
        raise Conflict('The selected profile is already executing')
    if row is None:
        return Verdict.CREATE
    if req.exclusive and facts.live and existing is None:
        raise Conflict('Codex environment is already executing a run')
    held = existing is not None or joined
    if row.mode is not req.mode and facts.live:
        raise Draining('Terminal mode changed while a run is active')
    for changed, message, blocked in (
            (row.template != req.template, 'Terminal template changed while a run is active',
             held or facts.live),
            (row.image != req.image, 'Terminal image changed while a run is active',
             held or facts.live),
            (row.runtime != req.runtime, 'Runtime mounts changed while a run is active',
             facts.live)):
        if changed:
            if blocked:
                raise Conflict(message)
            return Verdict.RETARGET
    drifted = row.fingerprint != req.fingerprint or row.mode is not req.mode
    if row.failure is not None and not held:
        over = facts.now - (row.failed_at or 0) >= facts.settings.failure_backoff
        if drifted or over:
            return Verdict.RETARGET
        raise BackendFailed(row.failure)
    if not held:
        if facts.live and (row.retire is not None or drifted):
            raise Draining('Environment is draining; retry when its current leases finish')
        if drifted or row.retire is Retire.CLOSED:
            return Verdict.RETARGET
    return Verdict.REUSE
