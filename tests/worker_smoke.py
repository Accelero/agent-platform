"""Check the real patched manager's single-profile handoff inside its image."""

import tempfile
from pathlib import Path

from manager import Conflict, Manager


class Backend:
    """Model delayed deletion acknowledgement without starting any worker."""

    def __init__(self) -> None:
        """Retain the number of live execution generations."""
        self.resources: set[str] = set()
        self.deletion_complete = False

    def ensure(
        self,
        name: str,
        _generation: str,
        _template: dict,
        _resource: dict,
    ) -> tuple:
        """Record provisioning and return a ready dummy endpoint."""
        self.resources.add(name)
        if len(self.resources) > 1:
            msg = "More than one worker generation was provisioned."
            raise RuntimeError(msg)
        return {"name": name}, {"host": "127.0.0.1", "port": 2222}

    def remove(self, resource: dict) -> bool:
        """Do not release a quota slot until the backend confirms deletion."""
        if self.deletion_complete:
            self.resources.discard(resource["name"])
        return self.deletion_complete


def refused(manager: Manager, payload: dict) -> None:
    """Require a concurrency conflict rather than silently granting another run."""
    try:
        manager.acquire(payload)
    except Conflict:
        return
    msg = "Manager granted a concurrent or not-yet-retired execution."
    raise RuntimeError(msg)


def main() -> None:
    """Exercise retries, live lease exclusion and idle worker replacement."""
    config = {
        "templates": {"personal": {"serialize_profile": True, "exclusive": True}},
        "deployments": {
            "fixture": {"profiles": {"personal": {"templates": ["personal"]}}},
        },
    }
    backend = Backend()
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
        manager = Manager(config, backend, str(Path(root) / "manager.db"))
        try:
            lease = manager.acquire(first)
            if manager.acquire(first)["lease"] != lease["lease"]:
                msg = "A retry replaced the native lease identity."
                raise RuntimeError(msg)
            refused(manager, {**first, "request": "duplicate-run"})
            refused(manager, second)
            manager.release(lease["lease"], reconcile=False)
            refused(manager, second)
            backend.deletion_complete = True
            manager.acquire(second)
        finally:
            manager.db.close()


if __name__ == "__main__":
    main()
