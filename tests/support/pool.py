"""One platform for all the destructive cases, replaced only when one is unusable.

A destructive case pauses the database, scales the controller, rotates keys or
suspends owners. Building a platform for each costs about 3.5 minutes, most of
the case's time, so the cases share one and the pool checks, after every case,
that the platform can serve the next one. A case that failed, or that left the
platform unhealthy, discards it; the next case builds a new one. Failures are
therefore never inherited: a case that breaks the platform is the last to use it.
"""

from __future__ import annotations

import time
from collections.abc import Callable
from typing import Any

from tests.support.ownership import Cleanup, docker

SETTLE_SECONDS = 240


class SharedPlatform:
    """`build(cleanup)` makes a platform and registers its finalizers;
    `check(platform)` returns why it cannot serve another case, or `None`."""

    def __init__(
        self,
        build: Callable[[Cleanup], Any],
        check: Callable[[Any], str | None],
    ):
        self._build, self._check = build, check
        self._cleanup: Cleanup | None = None
        self.platform: Any = None
        self.builds = 0
        self.discarded: list[str] = []

    def take(self):
        if self.platform is None:
            cleanup = Cleanup()
            try:
                self.platform = self._build(cleanup)
            except BaseException as error:
                cleanup.__exit__(type(error), error, error.__traceback__)
                raise
            self._cleanup = cleanup
            self.builds += 1
        return self.platform

    def release(self, passed: bool) -> None:
        reason = None if passed else "the case failed"
        if reason is None:
            reason = self._check(self.platform)
        if reason is not None:
            self.discarded.append(reason)
            self.close()

    def close(self) -> None:
        cleanup, self._cleanup, self.platform = self._cleanup, None, None
        if cleanup is not None:
            cleanup.__exit__(None, None, None)


def settled(check: Callable[[Any], str | None], platform, seconds=SETTLE_SECONDS):
    """`check` until it passes: a case may still be restoring what it broke."""
    deadline = time.monotonic() + seconds
    reason = "not checked"
    while time.monotonic() < deadline:
        try:
            reason = check(platform)
        except Exception as error:
            reason = f"the check raised {type(error).__name__}"
        if reason is None:
            return None
        time.sleep(5)
    return reason


def unusable(platform) -> str | None:
    """Why the platform cannot serve the next case, or `None`. It must have its
    database running, the provider answering, the platform's three workloads
    available, and the commissioned owners ready."""
    import json

    import requests

    from tests.personal_hermes import native_e2e as e2e

    def check(platform):
        state = json.loads(
            docker(["inspect", "--format", "{{json .State}}", platform.postgres])
        )
        if state["Paused"] or not state["Running"]:
            return "the database is not running"
        reply = requests.get(
            f"{e2e.ISSUER}/.well-known/openid-configuration",
            verify=platform.ca,
            timeout=5,
        )
        if reply.status_code != 200:
            return "the provider does not serve its realm"
        deployments = json.loads(
            e2e.kubectl(
                platform.identifier,
                "get",
                "deployments",
                "--namespace",
                e2e.NAMESPACE,
                "-o",
                "json",
            )
        )["items"]
        for deployment in deployments:
            wanted = deployment["spec"].get("replicas", 1)
            if deployment["status"].get("availableReplicas", 0) < wanted:
                return f"{deployment['metadata']['name']} is not available"
        for receipt in platform.assignments.values():
            if platform.admin.phase(receipt["instanceId"])["phase"] != "ready":
                return "a commissioned owner is not ready"
        return None

    return settled(check, platform)
