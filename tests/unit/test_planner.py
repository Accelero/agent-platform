"""The reconciliation planner decides from a record and a Pod alone (X1-X7, R3, R5,
R6).
"""

import itertools
from datetime import UTC, datetime

import pytest
from runtime_manager.domain import planner
from runtime_manager.domain.model import (
    Failure,
    Mode,
    PodState,
    Retire,
    Settings,
    Timers,
)
from runtime_manager.domain.planner import (
    CreatePod,
    DeletePod,
    EndLeases,
    Fail,
    Forget,
    MarkReady,
    MarkStarted,
    Replace,
    SetRetire,
    StartDraining,
)

from tests.support.builders import ENDPOINT, environment, failed, pod

pytestmark = pytest.mark.unit

NOW = 1_000_000.0
SETTINGS = Settings()
TIMERS = Timers(session_idle=60, on_demand_idle=60)


def plan(
    env, pod_=None, leases=0, *, wanted=False, fingerprint="fp", timers=TIMERS, now=NOW
):
    return planner.plan(env, pod_, leases, now, SETTINGS, timers, fingerprint, wanted)


def test_a_pod_that_is_starting_is_never_idle():
    """X1: idle timers do not apply before the Pod is Ready."""
    for mode in (Mode.PER_SESSION, Mode.PER_PROFILE_ON_DEMAND):
        starting = environment(mode=mode, idle_since=NOW - 3600, started=NOW - 10)
        assert plan(starting, pod(PodState.PENDING)) == []


def test_a_pod_that_just_became_ready_restarts_the_idle_clock_instead_of_expiring():
    """X1: the record was created long ago, but idle time counts from readiness."""
    for mode in (Mode.PER_SESSION, Mode.PER_PROFILE_ON_DEMAND):
        waited = environment(mode=mode, idle_since=NOW - 3600, started=NOW - 100)
        assert plan(waited, pod()) == [MarkReady(ENDPOINT)]


def test_a_pod_that_does_not_become_ready_in_time_fails_with_start_timeout():
    """X2: the deadline is start_timeout_seconds from the request."""
    env = environment(started=NOW - SETTINGS.start_timeout)
    assert plan(env, pod(PodState.PENDING)) == [Fail(Failure.START_TIMEOUT)]
    env = environment(started=NOW - SETTINGS.start_timeout + 1)
    assert plan(env, pod(PodState.PENDING)) == []


@pytest.mark.parametrize(
    ("observed", "expected"),
    [
        (failed(Failure.IMAGE_UNAVAILABLE), Failure.IMAGE_UNAVAILABLE),
        (failed(Failure.START_FAILED), Failure.START_FAILED),
        (pod(PodState.FAILED), Failure.START_FAILED),
    ],
)
def test_failures_are_recorded_with_their_reason(observed, expected):
    """X3."""
    assert plan(environment(started=NOW - 5), observed) == [Fail(expected)]


def test_an_unobserved_start_is_stamped_and_readiness_recorded_once():
    assert plan(environment(), pod(PodState.PENDING)) == [MarkStarted()]
    assert plan(environment(), pod()) == [MarkReady(ENDPOINT)]
    assert plan(environment(endpoint=ENDPOINT), pod()) == []


def test_a_pod_is_created_when_a_client_waits_or_a_resident_profile_needs_it():
    assert plan(environment(), None, wanted=True) == [CreatePod()]
    assert plan(environment(), None) == []
    resident = environment(mode=Mode.PER_PROFILE_ALWAYS_RUNNING, last_used=NOW - 10)
    assert plan(resident, None) == [CreatePod()]
    unused = environment(mode=Mode.PER_PROFILE_ALWAYS_RUNNING, last_used=NOW - 90000)
    assert plan(unused, None) == []


def test_a_failed_attempt_is_removed_then_kept_for_the_backoff_then_forgotten():
    """X4: failure is recorded once and ends only after the backoff or a config
    change.
    """
    env = environment(failure=Failure.START_FAILED, failed_at=NOW - 5)
    assert plan(env, pod(PodState.FAILED)) == [DeletePod()]
    assert plan(env, pod(PodState.TERMINATING)) == []
    assert plan(env, None) == []
    assert plan(environment(**{**vars_of(env), "failed_at": NOW - 60}), None) == [
        Forget()
    ]
    assert plan(env, None, fingerprint="changed") == [Forget()]


def vars_of(env):
    from dataclasses import fields

    return {field.name: getattr(env, field.name) for field in fields(env)}


def test_a_lost_worker_ends_its_leases_at_once_and_is_replaced():
    """X5: a Pod that was Ready and is gone invalidates the leases that used it."""
    env = environment(endpoint=ENDPOINT)
    assert plan(env, None, leases=2) == [
        SetRetire(Retire.POD_LOST),
        EndLeases("worker lost"),
        Replace("fp"),
    ]
    assert plan(env, pod(PodState.FAILED), leases=0) == [
        SetRetire(Retire.POD_LOST),
        DeletePod(),
    ]


def test_a_replacement_waits_for_leases_until_the_drain_deadline():
    """X7."""
    drifting = environment(endpoint=ENDPOINT, retire=Retire.CONFIG)
    assert plan(drifting, pod(), leases=1) == [StartDraining()]
    waiting = environment(
        endpoint=ENDPOINT,
        retire=Retire.CONFIG,
        draining_since=NOW - SETTINGS.drain_deadline + 1,
    )
    assert plan(waiting, pod(), leases=1) == []
    overdue = environment(
        endpoint=ENDPOINT,
        retire=Retire.CONFIG,
        draining_since=NOW - SETTINGS.drain_deadline,
    )
    assert plan(overdue, pod(), leases=1) == [EndLeases("drain deadline"), DeletePod()]


def test_configuration_drift_replaces_an_idle_worker():
    """R3: delete the Pod, then take a new generation under the new fingerprint."""
    env = environment(endpoint=ENDPOINT)
    assert plan(env, pod(), fingerprint="new") == [
        SetRetire(Retire.CONFIG),
        DeletePod(),
    ]
    retiring = environment(endpoint=ENDPOINT, retire=Retire.CONFIG)
    assert plan(retiring, pod(PodState.TERMINATING), fingerprint="new") == []
    assert plan(retiring, None, fingerprint="new") == [Replace("new")]


def test_the_daily_recycle_runs_once_per_day_and_not_for_a_new_pod():
    """R5."""
    timers = Timers(recycle_at="03:00", timezone="UTC", on_demand_idle=10**6)
    at = datetime(2026, 11, 2, 3, 30, tzinfo=UTC).timestamp()
    today = "2026-11-02"
    env = environment(endpoint=ENDPOINT, idle_since=at)
    assert plan(env, pod(), timers=timers, now=at) == [
        SetRetire(Retire.SCHEDULE, today),
        DeletePod(),
    ]
    served = environment(endpoint=ENDPOINT, idle_since=at, last_schedule=today)
    assert plan(served, pod(), timers=timers, now=at) == []
    fresh = environment(idle_since=at)
    assert plan(fresh, None, wanted=True, timers=timers, now=at) == [CreatePod(today)]
    early = datetime(2026, 11, 2, 2, 59, tzinfo=UTC).timestamp()
    assert plan(env, pod(), timers=timers, now=early) == []
    per_session = environment(mode=Mode.PER_SESSION, endpoint=ENDPOINT, idle_since=at)
    assert plan(per_session, pod(), timers=timers, now=at) == []


def test_idle_workers_retire_by_mode():
    """R6: on-demand Pods go after on_demand_idle; per-session records go after
    session_idle.
    """
    idle = environment(endpoint=ENDPOINT, idle_since=NOW - 60)
    assert plan(idle, pod()) == [SetRetire(Retire.IDLE), DeletePod()]
    assert plan(environment(endpoint=ENDPOINT, idle_since=NOW - 59), pod()) == []
    session = environment(mode=Mode.PER_SESSION, endpoint=ENDPOINT, idle_since=NOW - 60)
    assert plan(session, pod()) == [SetRetire(Retire.CLOSED), DeletePod()]
    assert plan(environment(mode=Mode.PER_SESSION, retire=Retire.CLOSED), None) == [
        Forget()
    ]
    always = environment(
        mode=Mode.PER_PROFILE_ALWAYS_RUNNING, endpoint=ENDPOINT, idle_since=0.0
    )
    assert plan(always, pod()) == []
    assert plan(idle, pod(), leases=1) == []


STATES = [None, *(pod(state) for state in PodState), failed(Failure.IMAGE_UNAVAILABLE)]


@pytest.mark.parametrize(
    ("mode", "observed", "leases", "retire", "failure", "ready_once"),
    list(
        itertools.product(
            Mode,
            STATES,
            (0, 2),
            (None, *Retire),
            (None, Failure.START_FAILED),
            (False, True),
        )
    ),
)
def test_every_state_plans_a_consistent_set_of_actions(
    mode, observed, leases, retire, failure, ready_once
):
    """The planner never pairs creation with deletion, never forgets or replaces a
    record whose Pod is still present, and never ends a lease without a reason."""
    env = environment(
        mode=mode,
        retire=retire,
        failure=failure,
        failed_at=NOW - 5 if failure else None,
        endpoint=ENDPOINT if ready_once else None,
        started=NOW - 5,
    )
    actions = plan(env, observed, leases, wanted=True)
    kinds = [type(action) for action in actions]
    assert not ({CreatePod, DeletePod} <= set(kinds))
    if observed is not None:
        assert Forget not in kinds
        assert Replace not in kinds
    if EndLeases in kinds:
        assert leases
    assert len(kinds) == len(set(kinds))
