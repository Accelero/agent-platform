"""The execution service against a simulated namespace and a fake clock (X1-X8,
X16, R1-R6, R10).
"""

import threading
from copy import deepcopy
from datetime import UTC, datetime

import pytest
from runtime_manager.domain.errors import (
    BackendFailed,
    Conflict,
    Draining,
    Invalid,
    LeaseExpired,
    LeaseNotLive,
    Pending,
)

from tests.support.world import CUSTOM_IMAGE, DEFAULT_IMAGE, default_config, make_world

pytestmark = pytest.mark.unit


def pod_of(reply: dict) -> str:
    return f"hermes-exec-{reply['environment']}-{reply['generation']}"


# X1 ---------------------------------------------------------------------------


@pytest.mark.parametrize("mode", ["per_profile_on_demand", "per_session"])
def test_a_cold_start_slower_than_the_idle_timers_is_not_torn_down(world, mode):
    """X1, reproduced: with the chart's 60 s idle defaults a 100 s boot used to be
    retired again and again (14 Pods in 180 s) and never granted."""
    world.kube.default_boot = 100
    granted = None
    for second in range(180):
        world.clock.advance(1)
        if second % 10 == 0:
            world.service.maintain()
        try:
            world.acquire(mode=mode)
            granted = second
            break
        except Pending:
            continue
    assert granted is not None and 99 <= granted <= 101
    assert len(world.kube.created()) == 1
    assert world.kube.deleted() == []


# X2-X4 ------------------------------------------------------------------------


def test_an_image_that_cannot_be_pulled_is_reported_with_its_reason_not_retried_blindly(
    world,
):
    """X3, X4."""
    world.kube.pull_errors = {DEFAULT_IMAGE}
    with pytest.raises(BackendFailed) as failure:
        world.acquire()
    assert failure.value.reason == "image_unavailable"
    assert world.environments()[0]["failure"] == "image_unavailable"
    created = len(world.kube.created())
    for _ in range(5):
        world.clock.advance(10)
        world.service.maintain()
        with pytest.raises(BackendFailed):
            world.acquire(request="again")
    assert len(world.kube.created()) == created  # within the backoff nothing is retried
    world.clock.advance(60)
    world.kube.pull_errors.clear()
    assert world.acquire_ready(request="after-backoff")["lease"]
    assert len(world.kube.created()) == created + 1


def test_maintenance_never_starts_a_pod_for_a_failed_record(world):
    """X4: once the backoff is over the record is forgotten and no Pod is created."""
    world.kube.crashing = {DEFAULT_IMAGE}
    with pytest.raises(BackendFailed) as failure:
        world.acquire()
    assert failure.value.reason == "start_failed"
    created = len(world.kube.created())
    world.clock.advance(61)
    world.service.maintain()
    assert world.environments() == []
    assert len(world.kube.created()) == created


def test_a_changed_image_ends_the_backoff_at_once(world):
    """X4."""
    world.kube.pull_errors = {DEFAULT_IMAGE}
    with pytest.raises(BackendFailed):
        world.acquire()
    reply = world.acquire_ready(image=CUSTOM_IMAGE, request="other-image")
    assert world.kube.images().count(CUSTOM_IMAGE) == 1
    assert reply["generation"]


def test_a_pod_that_never_becomes_ready_times_out_but_a_slow_one_does_not(world):
    """X2, X3: an unschedulable Pod is only pending until the start deadline."""
    world.kube.unschedulable = True
    with pytest.raises(Pending):
        world.acquire()
    world.clock.advance(299)
    with pytest.raises(Pending):
        world.acquire()
    world.clock.advance(1)
    with pytest.raises(BackendFailed) as failure:
        world.acquire()
    assert failure.value.reason == "start_timeout"


# X5 ---------------------------------------------------------------------------


@pytest.mark.parametrize("how", ["vanish", "fail"])
def test_a_lost_worker_ends_its_leases_and_is_replaced(world, how):
    """X5."""
    first = world.acquire()
    getattr(world.kube, how)(pod_of(first))
    world.service.maintain()
    with pytest.raises(LeaseExpired):
        world.post("/v1/leases/heartbeat", lease=first["lease"])
    with pytest.raises(LeaseNotLive):
        world.acquire()  # the same request never grants a second lease
    second = world.acquire_ready(request="replacement")
    assert second["generation"] != first["generation"]


# X6 ---------------------------------------------------------------------------


def test_a_lease_cannot_be_renewed_past_its_maximum_age():
    """X6."""
    import tempfile
    from pathlib import Path

    with tempfile.TemporaryDirectory() as directory:
        world = make_world(
            Path(directory),
            config=default_config(max_lease_seconds=300, lease_ttl_seconds=120),
        )
        try:
            lease = world.acquire()["lease"]
            for _ in range(2):
                world.clock.advance(100)
                world.post("/v1/leases/heartbeat", lease=lease)
            world.clock.advance(80)  # 280 s old
            world.post("/v1/leases/heartbeat", lease=lease)
            world.acquire()  # replaying the request renews but stays capped
            world.clock.advance(21)  # 301 s old: the cap, not the ttl, ends it
            with pytest.raises(LeaseExpired):
                world.post("/v1/leases/heartbeat", lease=lease)
        finally:
            world.close()


# X7 ---------------------------------------------------------------------------


def test_a_replacement_waits_for_leases_then_ends_them_at_the_drain_deadline():
    """X7."""
    import tempfile
    from pathlib import Path

    with tempfile.TemporaryDirectory() as directory:
        world = make_world(
            Path(directory), config=default_config(drain_deadline_seconds=300)
        )
        try:
            held = world.acquire(request="held")
            world.config["templates"]["terminal"]["kubernetes"]["resources"] = {
                "limits": {"cpu": "2"}
            }
            world.service.maintain()  # the configuration drifted: a replacement is due
            with pytest.raises(Draining):
                world.acquire(request="newcomer")
            for seconds in (100, 100, 99):  # a live client keeps renewing
                world.clock.advance(seconds)
                world.post("/v1/leases/heartbeat", lease=held["lease"])
                world.service.maintain()
                assert world.environments()[0]["leases"] == 1
            world.clock.advance(1)
            world.service.maintain()
            with pytest.raises(LeaseExpired):
                world.post("/v1/leases/heartbeat", lease=held["lease"])
            fresh = world.acquire_ready(request="newcomer")
            assert fresh["generation"] != held["generation"]
            assert pod_of(held) in world.kube.deleted()
        finally:
            world.close()


# X8 ---------------------------------------------------------------------------


def test_a_live_request_returns_its_lease_and_an_ended_one_is_refused(world):
    """X8."""
    first = world.acquire()
    assert world.acquire()["lease"] == first["lease"]
    world.post("/v1/leases/release", lease=first["lease"])
    with pytest.raises(LeaseNotLive):
        world.acquire()
    world.clock.advance(86400 + 1)
    world.service.maintain()
    assert world.acquire_ready()["lease"] != first["lease"]


def test_a_request_id_cannot_move_to_another_environment(world):
    """X8."""
    world.acquire(profile="one")
    with pytest.raises(Invalid, match="another environment"):
        world.acquire(profile="two")


# X16 --------------------------------------------------------------------------


def test_lease_operations_never_wait_for_a_cluster_call(world):
    """X16: a slow Pod creation for one profile blocks neither heartbeats, releases,
    status reads nor another profile's acquire."""
    held = world.acquire(profile="steady", request="steady")
    entered, proceed = threading.Event(), threading.Event()
    original = world.kube.kubectl

    def slow(*args, body=None):
        if (
            args[0] == "create"
            and body["spec"]["containers"][0]["image"] == CUSTOM_IMAGE
        ):
            entered.set()
            assert proceed.wait(10)
        return original(*args, body=body)

    world.cluster.kubectl = slow
    blocked = threading.Thread(
        target=lambda: world.acquire(profile="slow", request="slow", image=CUSTOM_IMAGE)
    )
    blocked.start()
    try:
        assert entered.wait(10)
        world.post("/v1/leases/heartbeat", lease=held["lease"])
        assert world.environments()
        assert world.acquire(profile="other", request="other")["lease"]
        world.post("/v1/leases/release", lease=held["lease"])
    finally:
        proceed.set()
        blocked.join(10)
    assert not blocked.is_alive()


# R1 ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "overrides",
    [
        {"image": ""},
        {"image": "bad image"},
        {"image": "image\x00"},
        {"image": [CUSTOM_IMAGE]},
        {"template": "unapproved"},
        {"mode": "forever"},
        {"runtime": {"pvc": "private-data"}},
    ],
)
def test_requests_the_manager_does_not_approve_never_reach_the_cluster(
    world, overrides
):
    """R1."""
    with pytest.raises(ValueError):
        world.acquire(**overrides)
    assert world.kube.calls == []


def test_any_profile_can_select_any_configured_template_and_image(world):
    """R1: trusted profiles choose templates and images without an allowlist."""
    world.config["templates"]["other"] = {
        "image": "elsewhere.example/default:1",
        "kubernetes": {"role": "other"},
    }
    world.acquire(
        profile="new-profile",
        template="other",
        image="another-registry.example/image:2",
    )
    assert world.kube.images() == ["another-registry.example/image:2"]
    world.acquire(profile="second-profile", template="other", request="second")
    assert world.kube.images()[-1] == "elsewhere.example/default:1"


def test_an_absent_image_uses_the_template_default(world):
    world.acquire()
    assert world.kube.images() == [DEFAULT_IMAGE]


# R2 ---------------------------------------------------------------------------


def test_a_profile_worker_serves_all_sessions_and_each_session_gets_its_own(world):
    """R2."""
    first = world.acquire(session="chat-a", request="a")
    second = world.acquire(session="chat-b", request="b")
    assert first["environment"] == second["environment"]
    assert first["lease"] != second["lease"]
    world.post("/v1/leases/release", lease=first["lease"])
    world.service.maintain()
    assert world.kube.deleted() == []  # the other lease keeps the shared Pod
    sessions = {
        name: world.acquire(mode="per_session", session=name, request=name)
        for name in ("task-a", "task-b", "task-c")
    }
    assert len({lease["environment"] for lease in sessions.values()}) == 3
    again = world.acquire(mode="per_session", session="task-a", request="a-2")
    assert again["environment"] == sessions["task-a"]["environment"]
    assert again["generation"] == sessions["task-a"]["generation"]


# R3 ---------------------------------------------------------------------------


def test_a_changed_image_waits_for_active_work_then_replaces_the_pod(world):
    """R3."""
    first = world.acquire(image=DEFAULT_IMAGE)
    for request in ("next", "request"):
        with pytest.raises(Conflict, match="image changed"):
            world.acquire(image=CUSTOM_IMAGE, request=request)
    world.post("/v1/leases/release", lease=first["lease"])
    second = world.acquire_ready(image=CUSTOM_IMAGE, request="next")
    assert second["generation"] != first["generation"]
    assert world.kube.deleted() == [pod_of(first)]
    assert world.kube.images() == [CUSTOM_IMAGE]


def test_a_changed_template_waits_for_active_work(world):
    """R3."""
    world.config["templates"]["other"] = deepcopy(world.config["templates"]["terminal"])
    first = world.acquire()
    with pytest.raises(Conflict, match="template changed"):
        world.acquire(template="other", request="next")
    world.post("/v1/leases/release", lease=first["lease"])
    second = world.acquire_ready(template="other", request="next")
    assert second["generation"] != first["generation"]
    assert (
        world.store.by_identity(world.store.environments()[0].identity).template
        == "other"
    )


def test_a_changed_template_configuration_replaces_an_idle_worker(world):
    """R3: a new chart release replaces idle Pods the same way."""
    first = world.acquire()
    world.post("/v1/leases/release", lease=first["lease"])
    world.config["templates"]["terminal"]["kubernetes"]["resources"] = {
        "limits": {"cpu": "2"}
    }
    world.service.maintain()
    second = world.acquire_ready(request="after-upgrade")
    assert second["generation"] != first["generation"]


# R4 ---------------------------------------------------------------------------


def test_serialized_exclusive_templates_allow_one_run_per_profile(world):
    """R4."""
    world.config["templates"]["terminal"].update(serialize_profile=True, exclusive=True)
    world.acquire(session="chat-a", request="a")
    with pytest.raises(Conflict, match="already executing"):
        world.acquire(session="chat-b", request="b")
    with pytest.raises(Conflict, match="already executing"):
        world.acquire(mode="per_session", session="task", request="task")


# R5 ---------------------------------------------------------------------------


def test_the_daily_recycle_replaces_a_profile_worker_once_per_day(world):
    """R5."""
    template = world.config["templates"]["terminal"]
    template.update(
        recycle={"at": "03:00", "timezone": "UTC"}, on_demand_idle_seconds=10**6
    )
    world.clock.now = datetime(2026, 11, 2, 2, 0, tzinfo=UTC).timestamp()
    first = world.acquire()
    world.post("/v1/leases/release", lease=first["lease"])
    world.clock.advance(1800)
    world.service.maintain()
    assert world.environments()[0]["generation"] == first["generation"]
    world.clock.advance(3600)  # 03:30
    world.service.maintain()
    second = world.acquire_ready(request="after-recycle")
    assert second["generation"] != first["generation"]
    world.post("/v1/leases/release", lease=second["lease"])
    world.clock.advance(3600)
    world.service.maintain()
    assert world.acquire_ready(request="same-day")["generation"] == second["generation"]
    world.clock.advance(86400)
    world.service.maintain()
    assert world.acquire_ready(request="next-day")["generation"] != second["generation"]


# R6 ---------------------------------------------------------------------------


def test_idle_workers_are_retired_and_closing_a_session_retires_its_pod(world):
    """R6."""
    shared = world.acquire()
    world.post("/v1/leases/release", lease=shared["lease"])
    world.clock.advance(61)
    world.service.maintain()
    assert world.kube.pods == {} and world.environments()[0]["phase"] == "absent"
    task = world.acquire(mode="per_session", session="task", request="task")
    world.post("/v1/sessions/close", profile="default", session="task")
    world.service.maintain()
    assert pod_of(task) in world.kube.pods  # a lease still holds it
    world.post("/v1/leases/release", lease=task["lease"])
    world.service.maintain()
    assert pod_of(task) not in world.kube.pods
    assert all(env["session"] != "task" for env in world.environments())


def test_an_always_running_profile_is_restarted_when_its_pod_is_lost(world):
    """R6."""
    first = world.acquire(mode="per_profile_always_running")
    world.post("/v1/leases/release", lease=first["lease"])
    world.kube.vanish(pod_of(first))
    world.service.maintain()
    environment = world.environments()[0]
    assert environment["generation"] != first["generation"]
    assert environment["phase"] == "ready"


# R10 --------------------------------------------------------------------------


def test_the_deployed_stack_can_replace_a_worker_deleted_outside_the_manager(world):
    """R10: the stack's own integration test deletes the Pod and acquires with
    another image.
    """
    payload = {
        "profile": "default",
        "session": "s",
        "mode": "per_profile_on_demand",
        "runtime": {},
    }
    first = world.acquire(request="one", **payload)
    assert set(first) == {"environment", "generation", "lease", "ttl", "ssh"}
    assert pod_of(first) in world.kube.pods
    world.post("/v1/leases/release", lease=first["lease"])
    world.kube.vanish(pod_of(first))
    second = world.acquire_ready(request="two", image=CUSTOM_IMAGE, **payload)
    assert second["generation"] != first["generation"]
    assert world.kube.images() == [CUSTOM_IMAGE]
    with pytest.raises(ValueError):
        world.acquire(template="missing-template", request="three")


# Restart ----------------------------------------------------------------------


def test_a_restarted_manager_keeps_leases_and_workers(world):
    """X12: state is in the database and the cluster, not in the process."""
    first = world.acquire(mode="per_profile_always_running", image=CUSTOM_IMAGE)
    created = len(world.kube.created())
    again = world.restart()
    try:
        again.service.maintain()
        assert (
            again.acquire(mode="per_profile_always_running", image=CUSTOM_IMAGE)[
                "lease"
            ]
            == first["lease"]
        )
        again.post("/v1/leases/heartbeat", lease=first["lease"])
        assert len(again.kube.created()) == created
    finally:
        again.close()


def test_a_replacement_waits_for_confirmed_pod_absence(world):
    """X4, X16: asynchronous deletion must not consume quota with two workers."""
    first = world.acquire()
    world.post("/v1/leases/release", lease=first["lease"])
    world.kube.terminate_after = 20
    with pytest.raises(Pending):
        world.acquire(image=CUSTOM_IMAGE, request="replacement")
    assert len(world.kube.created()) == 1
    assert world.environments()[0]["generation"] == first["generation"]
    world.clock.advance(19)
    with pytest.raises(Pending):
        world.acquire(image=CUSTOM_IMAGE, request="replacement")
    assert len(world.kube.created()) == 1
    world.clock.advance(1)
    fresh = world.acquire_ready(image=CUSTOM_IMAGE, request="replacement")
    assert fresh["generation"] != first["generation"]
    assert len(world.kube.created()) == 2


def test_serialization_retires_an_idle_sibling_before_starting_another(world):
    """R4: serialized profiles cannot leave idle siblings occupying the quota."""
    world.config["templates"]["terminal"]["serialize_profile"] = True
    first = world.acquire(mode="per_session", session="first")
    world.post("/v1/leases/release", lease=first["lease"])
    second = world.acquire(mode="per_session", session="second", request="second")
    assert pod_of(first) in world.kube.deleted()
    assert list(world.kube.pods) == [pod_of(second)]


def test_closing_a_session_can_filter_by_invocation_scope(world):
    """R6: when runtime is supplied only the matching invocation closes."""
    first_scope = {"hermes_home": "/opt/data/profiles/default", "workdir": "/workspace"}
    second_scope = {"hermes_home": "/opt/data/custom", "workdir": "/workspace"}
    world.config["templates"]["terminal"]["workdir_scope"] = {
        "controller_root": "/opt/data/.multica/workspaces"
    }
    first = world.acquire(mode="per_session", session="first", runtime=first_scope)
    second = world.acquire(
        mode="per_session", session="first", request="second", runtime=second_scope
    )
    # Filtering a mismatched scope leaves this session intact.
    world.post(
        "/v1/sessions/close", profile="default", session="first", runtime=second_scope
    )
    world.service.maintain()
    assert world.environments()[0]["retire"] is None
    world.post(
        "/v1/sessions/close", profile="default", session="first", runtime=first_scope
    )
    world.post("/v1/leases/release", lease=first["lease"])
    world.service.maintain()
    assert pod_of(first) not in world.kube.pods
    assert pod_of(second) in world.kube.pods


def test_a_concurrent_release_cannot_revive_an_acquire_request(world, monkeypatch):
    """X8: the grant transaction rechecks a lease ended after admission."""
    held = world.acquire()
    grant = world.store.grant

    def release_then_grant(*args):
        world.post("/v1/leases/release", lease=held["lease"])
        return grant(*args)

    monkeypatch.setattr(world.store, "grant", release_then_grant)
    with pytest.raises(LeaseNotLive):
        world.acquire()


def test_a_slow_cluster_observation_cannot_revive_an_expired_request(
    world, monkeypatch
):
    """X6, X8: time can pass after acquire's expiry sweep and before its grant."""
    world.acquire()
    observe = world.cluster.observe

    def slow_observation(name):
        world.clock.advance(121)
        return observe(name)

    monkeypatch.setattr(world.cluster, "observe", slow_observation)
    with pytest.raises(LeaseNotLive):
        world.acquire()


def test_a_changed_profile_mode_waits_for_live_work_even_on_replay(world):
    """R3: changing a shared worker's lifetime requires a fresh generation."""
    held = world.acquire()
    for request in ("request", "other"):
        with pytest.raises(Conflict, match="mode changed"):
            world.acquire(mode="per_profile_always_running", request=request)
    world.post("/v1/leases/release", lease=held["lease"])
    fresh = world.acquire_ready(mode="per_profile_always_running", request="other")
    assert fresh["generation"] != held["generation"]
    assert world.environments()[0]["mode"] == "per_profile_always_running"
