"""The destructive cases' shared platform: kept while healthy, replaced when not."""

import pytest

from tests.support import pool

pytestmark = pytest.mark.unit


class Built:
    """Stands in for a platform; records that its finalizer ran."""

    def __init__(self, number):
        self.number = number
        self.closed = False


def shared(check, built=None, fail=None):
    made = [] if built is None else built

    def build(cleanup):
        if fail:
            cleanup.add("half built", lambda: made.append("cleaned"))
            raise fail
        platform = Built(len(made) + 1)
        made.append(platform)
        cleanup.add("platform", lambda: setattr(platform, "closed", True))
        return platform

    return pool.SharedPlatform(build, check), made


def test_a_healthy_platform_serves_the_next_case_without_a_rebuild():
    platform, made = shared(lambda _: None)
    first = platform.take()
    platform.release(True)
    assert platform.take() is first
    platform.release(True)
    assert (platform.builds, first.closed) == (1, False)
    platform.close()
    assert first.closed
    assert platform.discarded == []


def test_a_failed_case_never_leaves_its_platform_to_the_next():
    platform, made = shared(lambda _: None)
    first = platform.take()
    platform.release(False)
    assert first.closed, "the broken platform was kept"
    assert platform.discarded == ["the case failed"]
    second = platform.take()
    assert second is not first and platform.builds == 2
    platform.close()


def test_a_platform_the_check_rejects_is_replaced_and_the_reason_kept():
    verdicts = iter(["the database is not running", None])
    platform, made = shared(lambda _: next(verdicts))
    first = platform.take()
    platform.release(True)
    assert first.closed and platform.discarded == ["the database is not running"]
    second = platform.take()
    platform.release(True)
    assert not second.closed and second is not first
    platform.close()
    assert second.closed


def test_closing_without_a_platform_is_harmless_and_repeatable():
    platform, _ = shared(lambda _: None)
    platform.close()
    platform.take()
    platform.close()
    platform.close()
    assert platform.builds == 1


def test_a_build_that_fails_runs_its_finalizers_and_leaves_nothing_taken():
    boom = RuntimeError("no cluster")
    platform, made = shared(lambda _: None, fail=boom)
    with pytest.raises(RuntimeError, match="no cluster"):
        platform.take()
    assert made == ["cleaned"] and platform.platform is None and platform.builds == 0


def test_a_finalizer_that_fails_when_a_platform_is_discarded_is_reported():
    def build(cleanup):
        cleanup.add("cluster", lambda: (_ for _ in ()).throw(OSError("leaked")))
        return Built(1)

    platform = pool.SharedPlatform(build, lambda _: None)
    platform.take()
    with pytest.raises(ExceptionGroup):
        platform.release(False)


def test_settling_waits_for_a_case_that_is_still_restoring_what_it_broke(monkeypatch):
    monkeypatch.setattr(pool.time, "sleep", lambda _: None)
    answers = iter(["still down", "still down", None])
    assert pool.settled(lambda _: next(answers), object(), seconds=60) is None


def test_settling_reports_the_last_reason_when_the_platform_never_recovers(monkeypatch):
    clock = iter(range(0, 1000, 20))
    monkeypatch.setattr(pool.time, "monotonic", lambda: next(clock))
    monkeypatch.setattr(pool.time, "sleep", lambda _: None)
    assert pool.settled(lambda _: "still down", object(), seconds=60) == "still down"


def test_a_check_that_raises_is_a_reason_not_a_crash(monkeypatch):
    clock = iter(range(0, 1000, 20))
    monkeypatch.setattr(pool.time, "monotonic", lambda: next(clock))
    monkeypatch.setattr(pool.time, "sleep", lambda _: None)

    def broken(_):
        raise ConnectionError("no answer")

    assert pool.settled(broken, object(), seconds=60) == (
        "the check raised ConnectionError"
    )
