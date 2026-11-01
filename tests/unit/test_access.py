"""Configuration and client isolation (R7)."""

from copy import deepcopy

import pytest
from runtime_manager.config import settings_from
from runtime_manager.domain.errors import Forbidden, LeaseExpired
from runtime_manager.domain.model import Settings

from tests.support.world import default_config, make_world

pytestmark = pytest.mark.unit


def test_settings_default_and_validate():
    assert settings_from({}) == Settings()
    assert settings_from({"drain_deadline_seconds": 90}).drain_deadline == 90
    for bad in (
        {"lease_ttl_seconds": 29},
        {"max_lease_seconds": 10},
        {"start_timeout_seconds": 0},
        {"failure_backoff_seconds": -1},
        {"lease_ttl_seconds": True},
        {"lease_ttl_seconds": "120"},
        {"orphan_policy": "adopt"},
        {"lease_ttl_seconds": 600, "max_lease_seconds": 300},
    ):
        with pytest.raises(ValueError):
            settings_from(bad)


def second_client(config):
    config = deepcopy(config)
    config["clients"]["codex"] = {"deployment": "other"}
    config["deployments"]["other"] = {}
    return config


def test_each_client_sees_only_its_own_leases_environments_and_request_ids(tmp_path):
    world = make_world(tmp_path, config=second_client(default_config()))
    try:
        mine = world.acquire()
        theirs = world.api.dispatch(
            "codex",
            "/v1/leases/acquire",
            {
                "profile": "default",
                "session": "s",
                "mode": "per_session",
                "request": "request",
            },
        )
        assert (
            mine["environment"] != theirs["environment"]
        )  # same request id, separate scopes
        assert [e["deployment"] for e in world.environments()] == ["fixture"]
        with pytest.raises(Forbidden):
            world.api.dispatch(
                "codex", "/v1/leases/heartbeat", {"lease": mine["lease"]}
            )
        with pytest.raises(Forbidden):
            world.api.dispatch("codex", "/v1/leases/release", {"lease": mine["lease"]})
        world.post("/v1/leases/heartbeat", lease=mine["lease"])  # still held
        with pytest.raises(LeaseExpired):
            world.api.dispatch("codex", "/v1/leases/heartbeat", {"lease": "0" * 32})
    finally:
        world.close()


@pytest.mark.parametrize(
    ("mutate", "message"),
    [
        (lambda c, t: t.update({"hermes": "short"}), "at least 32"),
        (lambda c, t: t.update({"codex": t["hermes"]}), "distinct"),
        (lambda c, t: t.pop("codex"), "separate credential"),
        (
            lambda c, t: c["clients"]["codex"].update(deployment="fixture"),
            "own deployment",
        ),
        (
            lambda c, t: c["clients"]["codex"].update(deployment="missing"),
            "not configured",
        ),
        (
            lambda c, t: c.update(default_template="missing"),
            "Default terminal template",
        ),
    ],
)
def test_credentials_must_be_distinct_long_and_bound_to_one_deployment(
    tmp_path, mutate, message
):
    from runtime_manager.api import ExecutionAPI

    world = make_world(tmp_path, config=second_client(default_config()))
    try:
        config, tokens = deepcopy(world.config), dict(world.tokens)
        mutate(config, tokens)
        world.service.config = config
        with pytest.raises(ValueError, match=message):
            ExecutionAPI(world.service, tokens)
    finally:
        world.close()


def test_only_the_exact_bearer_credential_authenticates(world):
    assert world.api.authenticate("Bearer " + world.tokens["hermes"]) == "hermes"
    for header in ("", "Bearer", "Bearer wrong", "Basic " + world.tokens["hermes"]):
        assert world.api.authenticate(header) is None
