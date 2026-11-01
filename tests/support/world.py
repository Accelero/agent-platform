"""The system under test, wired the way production wires it, with a fake clock and
namespace.
"""

import os
from dataclasses import dataclass
from pathlib import Path
from unittest import mock

from runtime_manager.api import ExecutionAPI
from runtime_manager.cluster import KubernetesCluster
from runtime_manager.domain.errors import Pending
from runtime_manager.service import Service
from runtime_manager.store import Store

from tests.support.fake_kube import NAMESPACE, FakeKube

DEFAULT_IMAGE = "ghcr.io/example/terminal:default"
CUSTOM_IMAGE = "ghcr.io/example/terminal:custom"


def token(client: str) -> str:
    """A distinct 32-character credential per client."""
    return (client * 32)[:32]


TOKEN = token("hermes")


class Clock:
    """Time that moves only when a test says so."""

    def __init__(self, start: float = 1_000_000.0):
        self.now = start

    def __call__(self) -> float:
        return self.now

    def advance(self, seconds: float) -> None:
        self.now += seconds


def default_config(**settings) -> dict:
    """One client and one template, with the chart's idle defaults of 60 seconds."""
    return {
        "clients": {"hermes": {"deployment": "fixture"}},
        "default_template": "terminal",
        "deployments": {"fixture": {}},
        "templates": {
            "terminal": {
                "image": DEFAULT_IMAGE,
                "session_idle_seconds": 60,
                "on_demand_idle_seconds": 60,
                "kubernetes": {"role": "hermes"},
            }
        },
        **settings,
    }


@dataclass
class World:
    clock: Clock
    kube: FakeKube
    cluster: KubernetesCluster
    store: Store
    service: Service
    api: ExecutionAPI
    config: dict
    path: Path
    tokens: dict

    def close(self) -> None:
        self.store.close()

    def restart(self) -> World:
        """A new manager process on the same database and the same namespace."""
        self.store.close()
        return make_world(
            self.path.parent,
            config=self.config,
            clock=self.clock,
            kube=self.kube,
            database=self.path,
        )

    def acquire(self, **overrides) -> dict:
        payload = {
            "profile": "default",
            "session": "session",
            "mode": "per_profile_on_demand",
            "request": "request",
        }
        return self.api.dispatch(
            "hermes", "/v1/leases/acquire", {**payload, **overrides}
        )

    def acquire_ready(
        self, *, step: float = 1.0, limit: int = 600, **overrides
    ) -> dict:
        """Retry as a client does, letting `step` seconds pass between attempts."""
        for _ in range(limit):
            try:
                return self.acquire(**overrides)
            except Pending:
                self.clock.advance(step)
        raise AssertionError("the worker never became ready")

    def post(self, path: str, **body):
        return self.api.dispatch("hermes", path, body)

    def environments(self) -> list[dict]:
        return self.api.dispatch("hermes", "/v1/environments")


def make_world(
    tmp_path: Path,
    *,
    config: dict | None = None,
    clock: Clock | None = None,
    kube: FakeKube | None = None,
    database: Path | None = None,
) -> World:
    config = config if config is not None else default_config()
    clock = clock or Clock()
    kube = kube or FakeKube(clock)
    with mock.patch.dict(os.environ):
        os.environ.pop("KUBERNETES_SERVICE_HOST", None)
        cluster = KubernetesCluster(
            config.get("kubernetes") or {"namespace": NAMESPACE}
        )
    cluster.kubectl = kube.kubectl
    path = database or tmp_path / "state.sqlite3"
    store = Store(str(path))
    service = Service(config, cluster, store, clock=clock)
    tokens = {name: token(name) for name in config["clients"]}
    api = ExecutionAPI(service, tokens)
    return World(clock, kube, cluster, store, service, api, config, path, tokens)
