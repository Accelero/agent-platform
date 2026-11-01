"""Acquire admission is a pure decision (X4, X8, R3, R4, R7)."""

from dataclasses import replace

import pytest
from runtime_manager.domain.admission import Facts, Request, Verdict, admit
from runtime_manager.domain.errors import (
    BackendFailed,
    Conflict,
    Draining,
    Invalid,
    LeaseNotLive,
)
from runtime_manager.domain.model import (
    Failure,
    Lease,
    LeaseState,
    Mode,
    Retire,
    Settings,
)

from tests.support.builders import ENDPOINT, environment

pytestmark = pytest.mark.unit

NOW = 5000.0


def request(**overrides) -> Request:
    base = Request(
        deployment="fixture",
        profile="default",
        session="s",
        template="terminal",
        mode=Mode.PER_PROFILE_ON_DEMAND,
        request="r1",
        image=None,
        runtime={},
        parent_lease="",
        fingerprint="fp",
    )
    return replace(base, **overrides)


def facts(row=None, **overrides) -> Facts:
    base = Facts(
        row=row,
        live=0,
        existing=None,
        parent=None,
        peers_busy=False,
        now=NOW,
        settings=Settings(),
    )
    return replace(base, **overrides)


def lease(state=LeaseState.ACTIVE, environment_id="a" * 24) -> Lease:
    return Lease("l1", environment_id, "r1", state, NOW - 10, NOW + 100)


def ready(**overrides):
    return environment(endpoint=ENDPOINT, **overrides)


def test_a_new_identity_creates_an_environment():
    assert admit(request(), facts()) is Verdict.CREATE


def test_an_unchanged_environment_is_reused_with_or_without_live_leases():
    assert admit(request(), facts(ready())) is Verdict.REUSE
    assert admit(request(), facts(ready(), live=3)) is Verdict.REUSE


def test_request_ids_are_idempotent_and_never_revive():
    """X8."""
    row = ready()
    held = facts(row, live=1, existing=lease())
    assert admit(request(), held) is Verdict.REUSE
    with pytest.raises(LeaseNotLive):
        admit(request(), facts(row, existing=lease(LeaseState.RELEASED)))
    with pytest.raises(LeaseNotLive):
        admit(request(), facts(None, existing=lease(LeaseState.EXPIRED)))
    with pytest.raises(Invalid, match="another environment"):
        admit(request(), facts(row, existing=lease(environment_id="c" * 24)))
    with pytest.raises(Invalid, match="another environment"):
        admit(request(), facts(None, existing=lease()))


def test_a_parent_lease_must_own_the_environment():
    row = ready()
    with pytest.raises(Invalid, match="Parent lease"):
        admit(request(parent_lease="l1"), facts(row, live=1))
    owner = lease()
    assert (
        admit(request(parent_lease="l1"), facts(row, live=1, parent=owner))
        is Verdict.REUSE
    )
    with pytest.raises(Invalid, match="Parent lease"):
        admit(
            request(parent_lease="l1"),
            facts(row, live=1, parent=lease(LeaseState.RELEASED)),
        )


@pytest.mark.parametrize(
    ("change", "message"),
    [
        ({"template": "other"}, "template changed"),
        ({"image": "elsewhere:1"}, "image changed"),
    ],
)
def test_a_changed_template_or_image_waits_for_leases_and_replay(change, message):
    """R3."""
    row = ready()
    with pytest.raises(Conflict, match=message):
        admit(request(**change), facts(row, live=1))
    with pytest.raises(Conflict, match=message):
        admit(request(**change), facts(row, live=1, existing=lease()))
    assert admit(request(**change), facts(row)) is Verdict.RETARGET


def test_changed_runtime_mounts_wait_for_leases():
    """R3."""
    row = ready(runtime={"workdir": "/workspace"})
    with pytest.raises(Conflict, match="Runtime mounts changed"):
        admit(request(runtime={"workdir": "/other"}), facts(row, live=1))
    assert admit(request(runtime={"workdir": "/other"}), facts(row)) is Verdict.RETARGET


def test_drift_drains_while_leases_live_and_retargets_when_idle():
    """R3."""
    for change in ({"fingerprint": "new"}, {"mode": Mode.PER_PROFILE_ALWAYS_RUNNING}):
        with pytest.raises(Draining):
            admit(request(**change), facts(ready(), live=1))
        assert admit(request(**change), facts(ready())) is Verdict.RETARGET
    joined = facts(ready(), live=1, existing=lease())
    assert admit(request(fingerprint="new"), joined) is Verdict.REUSE


def test_a_retiring_environment_is_reused_unless_it_is_closing_or_busy():
    assert admit(request(), facts(ready(retire=Retire.IDLE))) is Verdict.REUSE
    assert admit(request(), facts(ready(retire=Retire.CLOSED))) is Verdict.RETARGET
    with pytest.raises(Draining):
        admit(request(), facts(ready(retire=Retire.LEASE_LOST), live=1))


def test_a_failed_attempt_is_reported_until_the_backoff_ends_or_the_config_changes():
    """X4."""
    row = environment(failure=Failure.IMAGE_UNAVAILABLE, failed_at=NOW - 5)
    with pytest.raises(BackendFailed) as failure:
        admit(request(), facts(row))
    assert failure.value.reason == "image_unavailable"
    assert admit(request(fingerprint="new"), facts(row)) is Verdict.RETARGET
    over = environment(failure=Failure.START_FAILED, failed_at=NOW - 60)
    assert admit(request(), facts(over)) is Verdict.RETARGET


def test_serialized_and_exclusive_templates_allow_one_run():
    """R4."""
    with pytest.raises(Conflict, match="already executing"):
        admit(request(serialize=True), facts(peers_busy=True))
    assert admit(request(serialize=True), facts()) is Verdict.CREATE
    with pytest.raises(Conflict, match="already executing"):
        admit(request(exclusive=True), facts(ready(), live=1))
    held = facts(ready(), live=1, existing=lease())
    assert admit(request(exclusive=True), held) is Verdict.REUSE
