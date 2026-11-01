"""Hermes client -> TLS -> SQLite/service -> Kubernetes adapter -> namespace double."""

import threading
from contextlib import contextmanager
from dataclasses import replace

import pytest

from tests.support.http import serve
from tests.support.plugin import Ticks, client_settings, load_plugin
from tests.support.world import DEFAULT_IMAGE

pytestmark = pytest.mark.e2e


@contextmanager
def ticking(world):
    stop = threading.Event()

    def run():
        while not stop.wait(0.01):
            world.clock.advance(10)
            world.service.maintain()

    thread = threading.Thread(target=run, daemon=True)
    thread.start()
    try:
        yield
    finally:
        stop.set()
        thread.join(5)
        assert not thread.is_alive()


def test_cold_start_through_the_plugin_survives_idle_timers(
    monkeypatch, world, tls, tmp_path
):
    """X1: the real client eventually receives the same slow worker."""
    plugin = load_plugin(monkeypatch)
    world.kube.default_boot = 100
    with serve(world, tls) as served, ticking(world):
        reply = plugin.Client(client_settings(served, tmp_path)).acquire("cold")
        assert reply["lease"]
    assert len(world.kube.created()) == 1
    assert world.kube.deleted() == []


def test_pull_failure_reaches_the_plugin_then_recovers_after_backoff(
    monkeypatch, world, tls, tmp_path
):
    """X3, X4, X15: a permanent refusal does not spend the retry window."""
    plugin = load_plugin(monkeypatch)
    world.kube.pull_errors.add(DEFAULT_IMAGE)
    with serve(world, tls) as served:
        client = plugin.Client(client_settings(served, tmp_path))
        with pytest.raises(RuntimeError, match="image_unavailable"):
            client.acquire("broken")
        assert len(world.kube.created()) == 1
        world.kube.pull_errors.clear()
        world.clock.advance(61)
        assert client.acquire("recovered")["lease"]
        assert len(world.kube.created()) == 2


@pytest.mark.parametrize("cause", ["maximum_age", "drain", "pod_lost"])
def test_forced_lease_ends_reach_the_plugins_heartbeat(
    monkeypatch, world, tls, tmp_path, cause
):
    """X5, X6, X7: definitive heartbeats stop the plugin at once."""
    plugin = load_plugin(monkeypatch)
    world.service.settings = replace(
        world.service.settings, max_lease=60, drain_deadline=30
    )
    with serve(world, tls) as served:
        client = plugin.Client(client_settings(served, tmp_path))
        reply = client.acquire("held")
        if cause == "maximum_age":
            world.clock.advance(60)
        elif cause == "drain":
            world.config["templates"]["terminal"]["kubernetes"]["resources"] = {
                "limits": {"cpu": "3"}
            }
            world.service.maintain()
            world.clock.advance(30)
            world.service.maintain()
        else:
            world.kube.vanish(
                f"hermes-exec-{reply['environment']}-{reply['generation']}"
            )
            world.service.maintain()
        env = object.__new__(plugin.ManagedSSHEnvironment)
        env.client, env._lost = client, False
        stop = Ticks(lambda seconds: None, limit=1)
        env._renew(reply, stop)
        assert env._lost
        if cause == "pod_lost":
            fresh = client.acquire("replacement")
            assert fresh["generation"] != reply["generation"]


def test_restarting_the_manager_keeps_the_clients_lease(
    monkeypatch, world, tls, tmp_path
):
    """X12: a new process reuses the persisted grant and worker."""
    plugin = load_plugin(monkeypatch)
    with serve(world, tls) as served:
        client = plugin.Client(client_settings(served, tmp_path))
        reply = client.acquire("survives")
    again = world.restart()
    try:
        with serve(again, tls) as served:
            client = plugin.Client(client_settings(served, tmp_path))
            assert (
                client.call("/v1/leases/heartbeat", {"lease": reply["lease"]})["ttl"]
                > 0
            )
            fresh = client.acquire("survives")
            assert fresh["generation"] == reply["generation"]
        assert len(again.kube.created()) == 1
    finally:
        again.close()


def test_orphaned_workers_are_visible_to_the_client(monkeypatch, world, tls, tmp_path):
    """X11: a worker left after state loss is reported, never adopted."""
    plugin = load_plugin(monkeypatch)
    name = "hermes-exec-" + "a" * 24 + "-" + "b" * 12
    world.cluster.create(
        name, "a" * 24, world.service.catalog.resolve("fixture", "terminal").template
    )
    world.clock.advance(300)
    world.service.scan_orphans()
    with serve(world, tls) as served:
        client = plugin.Client(client_settings(served, tmp_path))
        assert client.call("/v1/orphans")[0]["name"] == name
    assert not world.kube.deleted()
