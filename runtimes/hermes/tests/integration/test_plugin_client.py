"""The shipped Hermes client over real HTTPS (X15, old-manager compatibility)."""

import pytest
from runtime_manager.domain.errors import (
    BackendFailed,
    Conflict,
    LeaseNotLive,
    ManagerError,
)

from tests.support.http import serve
from tests.support.plugin import client_settings, load_plugin


@pytest.mark.parametrize(
    "code",
    ["environment_pending", "environment_draining", "conflict", "unavailable", ""],
)
def test_plugin_retries_temporary_refusals(monkeypatch, world, tls, tmp_path, code):
    plugin = load_plugin(monkeypatch)
    calls = []
    if not code:
        # The previous manager had no `code` field at all.
        from runtime_manager import server

        error_body = server.error_body

        def legacy_error(error):
            status, body, headers = error_body(error)
            body.pop("code", None)
            return status, body, headers

        monkeypatch.setattr(server, "error_body", legacy_error)

    def dispatch(client, path, body):
        calls.append(body)
        if len(calls) < 3:
            error = (
                ManagerError("registry recovering")
                if code == "unavailable"
                else Conflict("worker still starting")
            )
            error.code = code
            raise error
        return {"lease": "granted"}

    monkeypatch.setattr(world.api, "dispatch", dispatch)
    with serve(world, tls) as served:
        reply = plugin.Client(client_settings(served, tmp_path)).acquire("session")
    assert reply == {"lease": "granted"}
    assert len(calls) == 3
    assert len({body["request"] for body in calls}) == 1


@pytest.mark.parametrize(
    "error",
    [BackendFailed("image_unavailable"), LeaseNotLive("the original lease ended")],
)
def test_plugin_fails_at_once_with_the_managers_message(
    monkeypatch, world, tls, tmp_path, error
):
    plugin = load_plugin(monkeypatch)
    calls = []

    def dispatch(*args):
        calls.append(args)
        raise error

    monkeypatch.setattr(world.api, "dispatch", dispatch)
    with serve(world, tls) as served:
        with pytest.raises(RuntimeError, match=str(error)):
            plugin.Client(client_settings(served, tmp_path)).acquire("session")
    assert len(calls) == 1


def test_plugin_timeout_keeps_the_last_server_message(
    monkeypatch, world, tls, tmp_path
):
    plugin = load_plugin(monkeypatch)

    def dispatch(*args):
        raise ManagerError("worker quota is full")

    monkeypatch.setattr(world.api, "dispatch", dispatch)
    with serve(world, tls) as served:
        settings = client_settings(served, tmp_path, acquire_timeout_seconds=0.05)
        with pytest.raises(RuntimeError, match="worker quota is full"):
            plugin.Client(settings).acquire("session")
