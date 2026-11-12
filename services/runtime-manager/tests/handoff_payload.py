"""Check the manager's serialized worker handoff; run in process and in its image."""

import tempfile
from pathlib import Path

from runtime_manager.domain.errors import Conflict
from runtime_manager.domain.model import Pod, PodState
from runtime_manager.service import Service
from runtime_manager.store import Store


class Cluster:
    """Model delayed deletion acknowledgement without starting any worker."""

    def __init__(self) -> None:
        self.pods: dict[str, Pod] = {}
        self.deletion_complete = False

    def create(self, name: str, environment: str, template: dict) -> None:
        self.pods[name] = Pod(
            name,
            PodState.READY,
            endpoint={
                "host": "127.0.0.1",
                "port": 2222,
                "user": "worker",
            },
        )
        if len(self.pods) > 1:
            raise RuntimeError("More than one worker generation was provisioned.")

    def observe(self, name: str) -> Pod | None:
        pod = self.pods.get(name)
        if pod and pod.state is PodState.TERMINATING and self.deletion_complete:
            del self.pods[name]
            return None
        return pod

    def delete(self, name: str) -> None:
        self.pods[name] = Pod(name, PodState.TERMINATING)


def refused(service: Service, payload: dict) -> None:
    """Require a conflict instead of silently granting another run."""
    try:
        service.acquire(payload)
    except Conflict:
        return
    raise RuntimeError("Manager granted a concurrent or not-yet-retired execution.")


def main() -> None:
    """Exercise retries, live lease exclusion and idle worker replacement."""
    config = {
        "templates": {
            "personal": {
                "image": "ghcr.io/example/worker:fixture",
                "serialize_profile": True,
                "exclusive": True,
            }
        },
        "deployments": {"fixture": {}},
    }
    cluster = Cluster()
    first = {
        "deployment": "fixture",
        "profile": "personal",
        "template": "personal",
        "session": "first",
        "request": "first",
        "mode": "per_session",
    }
    second = {**first, "session": "second", "request": "second"}
    with tempfile.TemporaryDirectory() as root:
        store = Store(str(Path(root) / "manager.db"))
        service = Service(config, cluster, store)
        try:
            lease = service.acquire(first)
            if service.acquire(first)["lease"] != lease["lease"]:
                raise RuntimeError("A retry replaced the native lease identity.")
            refused(service, {**first, "request": "duplicate-run"})
            refused(service, second)
            service.release(lease["lease"], reconcile=False)
            refused(service, second)
            cluster.deletion_complete = True
            fresh = service.acquire(second)
            if fresh["environment"] == lease["environment"] or len(cluster.pods) != 1:
                raise RuntimeError("The serialized worker handoff did not finish.")
        finally:
            store.close()


if __name__ == "__main__":
    main()
