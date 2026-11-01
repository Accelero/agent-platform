"""Lifecycle vocabulary: typed states and the records the planner reads.

Nothing here performs I/O. The store persists these records, the cluster adapter
produces `Pod` observations and the planner decides from both.
"""
from __future__ import annotations

import enum
import re
from dataclasses import dataclass


class Mode(enum.StrEnum):
    PER_SESSION = 'per_session'
    PER_PROFILE_ON_DEMAND = 'per_profile_on_demand'
    PER_PROFILE_ALWAYS_RUNNING = 'per_profile_always_running'


class Failure(enum.StrEnum):
    """Why a worker attempt could not start; recorded once per attempt."""
    IMAGE_UNAVAILABLE = 'image_unavailable'
    START_FAILED = 'start_failed'
    START_TIMEOUT = 'start_timeout'


class Retire(enum.StrEnum):
    """Why the current Pod must be replaced. `closed` also drops the record."""
    CLOSED = 'closed'
    CONFIG = 'config'
    SCHEDULE = 'schedule'
    IDLE = 'idle'
    LEASE_LOST = 'lease_lost'
    POD_LOST = 'pod_lost'


class PodState(enum.StrEnum):
    PENDING = 'pending'
    READY = 'ready'
    TERMINATING = 'terminating'
    FAILED = 'failed'


class LeaseState(enum.StrEnum):
    ACTIVE = 'active'
    RELEASED = 'released'
    EXPIRED = 'expired'
    REVOKED = 'revoked'


POD_NAME = re.compile(r'hermes-exec-[0-9a-f]{24}-[0-9a-f]{12}')


def pod_name(environment: str, generation: str) -> str:
    """The Pod of a generation. Persisting the generation first makes a crash
    between creating the Pod and saving it harmless."""
    return f'hermes-exec-{environment}-{generation}'


@dataclass(frozen=True, slots=True)
class Pod:
    """What the cluster shows for a worker Pod."""
    name: str
    state: PodState
    endpoint: dict | None = None
    failure: Failure | None = None
    created: float = 0.0


@dataclass(frozen=True, slots=True)
class Environment:
    """The record of one worker identity. At most one Pod belongs to it."""
    id: str
    identity: str
    deployment: str
    profile: str
    session: str
    template: str
    mode: Mode
    image: str | None
    runtime: dict
    fingerprint: str
    generation: str
    endpoint: dict | None = None
    started: float | None = None
    last_used: float = 0.0
    idle_since: float | None = None
    failure: Failure | None = None
    failed_at: float | None = None
    retire: Retire | None = None
    draining_since: float | None = None
    last_schedule: str = ''

    @property
    def pod_name(self) -> str:
        return pod_name(self.id, self.generation)


@dataclass(frozen=True, slots=True)
class Lease:
    id: str
    environment: str
    request: str
    state: LeaseState
    created: float
    expires: float


@dataclass(frozen=True, slots=True)
class Settings:
    """Manager-wide timers, all in seconds."""
    lease_ttl: int = 120
    max_lease: int = 86400
    start_timeout: int = 300
    failure_backoff: int = 60
    drain_deadline: int = 3600
    request_memory: int = 86400
    orphan_grace: int = 300
    orphan_scan: int = 60
    orphan_policy: str = 'report'
    maintenance_interval: int = 10


@dataclass(frozen=True, slots=True)
class Timers:
    """Per-template lifetime rules."""
    session_idle: int = 86400
    on_demand_idle: int = 60
    inactive_after: int = 86400
    recycle_at: str = ''
    timezone: str = 'Europe/Berlin'
