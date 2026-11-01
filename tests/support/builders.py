"""Records for tests of the pure domain."""

from dataclasses import replace

from runtime_manager.domain.model import (
    Environment,
    Failure,
    Mode,
    Pod,
    PodState,
)

ENDPOINT = {"host": "10.0.0.7", "port": 2222, "user": "worker"}


def environment(**overrides) -> Environment:
    base = Environment(
        id="a" * 24,
        identity='["fixture", "default", "profile"]',
        deployment="fixture",
        profile="default",
        session="",
        template="terminal",
        mode=Mode.PER_PROFILE_ON_DEMAND,
        image=None,
        runtime={},
        fingerprint="fp",
        generation="b" * 12,
    )
    return replace(base, **overrides)


def pod(state: PodState = PodState.READY, **overrides) -> Pod:
    values = {"name": "hermes-exec-" + "a" * 24 + "-" + "b" * 12, "state": state}
    if state is PodState.READY:
        values["endpoint"] = ENDPOINT
    return Pod(**{**values, **overrides})


def failed(failure: Failure) -> Pod:
    return pod(PodState.PENDING, failure=failure)
