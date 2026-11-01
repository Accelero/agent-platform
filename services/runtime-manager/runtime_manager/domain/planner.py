"""The reconciliation planner: a pure function from record and observation to actions.

`plan` compares what the record wants with what the cluster shows and returns the
difference as actions. It performs no I/O and remembers nothing, so a restarted
manager decides exactly as the one before it. The service executes the actions
and plans again until nothing is left to do.

Rule order:
  1. A failed attempt waits out its backoff, then the record is forgotten.
  2. A reason to replace the Pod is derived (drift, recycle, lost worker, idle).
  3. Live leases delay a replacement until the drain deadline; a lost worker's
     leases end at once.
  4. With no leases the Pod is deleted, then the record is replaced or forgotten.
  5. Without a reason the Pod is created when wanted, watched against the start
     deadline and its waiting reason, and recorded once Ready. A Pod that is
     still starting is never idle.
"""
from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, time as wall_time
from zoneinfo import ZoneInfo

from .model import (Environment, Failure, Mode, Pod, PodState, Retire, Settings,
                    Timers)


@dataclass(frozen=True, slots=True)
class SetRetire:
    reason: Retire
    day: str = ''


@dataclass(frozen=True, slots=True)
class StartDraining:
    pass


@dataclass(frozen=True, slots=True)
class EndLeases:
    why: str


@dataclass(frozen=True, slots=True)
class CreatePod:
    day: str = ''


@dataclass(frozen=True, slots=True)
class DeletePod:
    pass


@dataclass(frozen=True, slots=True)
class MarkStarted:
    pass


@dataclass(frozen=True, slots=True)
class MarkReady:
    endpoint: dict


@dataclass(frozen=True, slots=True)
class Fail:
    failure: Failure


@dataclass(frozen=True, slots=True)
class Replace:
    fingerprint: str


@dataclass(frozen=True, slots=True)
class Forget:
    pass


Action = (SetRetire | StartDraining | EndLeases | CreatePod | DeletePod
          | MarkStarted | MarkReady | Fail | Replace | Forget)


def schedule_day(now: float, timers: Timers, last: str) -> str:
    """The calendar date whose recycle boundary has passed and not yet been served.

    The date is persisted by the caller, so a restart or a repeated local hour at
    a daylight-saving change never recycles twice.
    """
    if not timers.recycle_at:
        return ''
    local = datetime.fromtimestamp(now, ZoneInfo(timers.timezone))
    hour, minute = map(int, timers.recycle_at.split(':'))
    day = local.date().isoformat()
    if local.time() >= wall_time(hour, minute) and last != day:
        return day
    return ''


def plan(env: Environment, pod: Pod | None, leases: int, now: float,
         settings: Settings, timers: Timers, fingerprint: str,
         wanted: bool) -> list[Action]:
    """Actions that bring `pod` toward what `env` wants.

    `leases` counts live leases, `fingerprint` is the current configuration of the
    environment's template and `wanted` says that a client is waiting for a Pod.
    """
    if env.failure is not None:
        return _after_failure(env, pod, leases, now, settings, fingerprint)
    day = '' if env.mode is Mode.PER_SESSION else schedule_day(now, timers, env.last_schedule)
    retire = _reason(env, pod, leases, now, timers, fingerprint, day)
    actions: list[Action] = []
    if retire is not env.retire:
        actions.append(SetRetire(retire, day if retire is Retire.SCHEDULE else ''))
    if retire is None:
        return actions + _keep(env, pod, now, settings, timers, day, wanted)
    if leases:
        if retire is Retire.POD_LOST:
            actions.append(EndLeases('worker lost'))
        elif env.draining_since is None:
            return actions + [StartDraining()]
        elif now - env.draining_since >= settings.drain_deadline:
            actions.append(EndLeases('drain deadline'))
        else:
            return actions
    return actions + _replace(pod, retire, fingerprint)


def _after_failure(env: Environment, pod: Pod | None, leases: int, now: float,
                   settings: Settings, fingerprint: str) -> list[Action]:
    if pod is not None:
        return [] if pod.state is PodState.TERMINATING else [DeletePod()]
    over = now - (env.failed_at or 0) >= settings.failure_backoff
    if (over or fingerprint != env.fingerprint) and not leases:
        return [Forget()]
    return []


def _reason(env: Environment, pod: Pod | None, leases: int, now: float,
            timers: Timers, fingerprint: str, day: str) -> Retire | None:
    if env.retire is not None:
        return env.retire
    if fingerprint != env.fingerprint:
        return Retire.CONFIG
    if day and (pod is not None or env.endpoint is not None):
        return Retire.SCHEDULE
    lost = pod is None or pod.state in (PodState.FAILED, PodState.TERMINATING)
    if env.endpoint is not None and lost:
        return Retire.POD_LOST
    # Idle time counts from readiness, so a Pod only becomes idle once its
    # readiness is recorded (MarkReady restarts the clock).
    settled = pod is None or (pod.state is PodState.READY and env.endpoint == pod.endpoint)
    if not leases and settled:
        idle = now - (env.idle_since if env.idle_since is not None else now)
        if env.mode is Mode.PER_SESSION and idle >= timers.session_idle:
            return Retire.CLOSED
        if (env.mode is Mode.PER_PROFILE_ON_DEMAND and pod is not None
                and idle >= timers.on_demand_idle):
            return Retire.IDLE
    return None


def _replace(pod: Pod | None, retire: Retire, fingerprint: str) -> list[Action]:
    if pod is not None:
        return [] if pod.state is PodState.TERMINATING else [DeletePod()]
    return [Forget()] if retire is Retire.CLOSED else [Replace(fingerprint)]


def _resident(env: Environment, timers: Timers, now: float) -> bool:
    """An always-running profile keeps its Pod while it was used recently."""
    return (env.mode is Mode.PER_PROFILE_ALWAYS_RUNNING
            and now - env.last_used < timers.inactive_after)


def _keep(env: Environment, pod: Pod | None, now: float, settings: Settings,
          timers: Timers, day: str, wanted: bool) -> list[Action]:
    if pod is None:
        return [CreatePod(day)] if wanted or _resident(env, timers, now) else []
    if pod.state is PodState.PENDING:
        if pod.failure is not None:
            return [Fail(pod.failure)]
        if env.started is None:
            return [MarkStarted()]
        if now - env.started >= settings.start_timeout:
            return [Fail(Failure.START_TIMEOUT)]
        return []
    if pod.state is PodState.READY:
        return [MarkReady(pod.endpoint)] if env.endpoint != pod.endpoint else []
    if pod.state is PodState.FAILED:
        return [Fail(pod.failure or Failure.START_FAILED)]
    return []
