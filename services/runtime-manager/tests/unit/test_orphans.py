"""Unknown workers are reported once and deleted only by opt-in policy (X11)."""

from dataclasses import replace

ORPHAN = "hermes-exec-" + "a" * 24 + "-" + "b" * 12


def add_orphan(world):
    world.cluster.create(
        ORPHAN, "a" * 24, world.service.catalog.resolve("fixture", "terminal").template
    )


def test_orphans_obey_grace_are_reported_once_and_clear_when_gone(world, caplog):
    add_orphan(world)
    assert world.service.scan_orphans() == []
    world.clock.advance(299)
    assert world.service.scan_orphans() == []
    world.clock.advance(1)
    report = world.service.scan_orphans()
    assert report == [
        {"name": ORPHAN, "created": world.clock() - 300, "first_seen": world.clock()}
    ]
    world.clock.advance(10)
    assert world.service.scan_orphans() == report
    assert (
        sum("belongs to no environment record" in r.message for r in caplog.records)
        == 1
    )
    assert not world.kube.deleted()
    assert world.environments() == []  # never adopted
    world.kube.vanish(ORPHAN)
    assert world.service.scan_orphans() == []


def test_a_named_worker_is_never_an_orphan(world):
    world.acquire()
    world.clock.advance(1000)
    assert world.service.scan_orphans() == []
    assert not world.kube.deleted()


def test_orphan_deletion_is_opt_in_and_waits_for_the_grace(world):
    world.service.settings = replace(world.service.settings, orphan_policy="delete")
    add_orphan(world)
    world.clock.advance(299)
    world.service.scan_orphans()
    assert not world.kube.deleted()
    world.clock.advance(1)
    assert world.service.scan_orphans()[0]["name"] == ORPHAN
    assert world.kube.deleted() == [ORPHAN]
    assert world.service.scan_orphans() == []


def test_an_acquire_during_orphan_listing_is_never_deleted(world, monkeypatch):
    """X11: a concurrent acquire persists its record before creating its Pod."""
    world.service.settings = replace(
        world.service.settings, orphan_grace=0, orphan_policy="delete"
    )
    original = world.cluster.list_managed

    def listing():
        world.acquire()
        return original()

    monkeypatch.setattr(world.cluster, "list_managed", listing)
    assert world.service.scan_orphans() == []
    assert not world.kube.deleted()
    assert len(world.kube.pods) == 1
