"""The three terminal lifetime modes against real workers (R9, R12).

One person per mode, each with a backend whose idle timeout is 60 s. The
plugin in each person's Hermes opens two sessions and releases them, then:

- `per_session` keys an environment by session and retires it when its session
  ends, so a new session starts a new one;
- `per_profile_on_demand` serves every session of the profile from one
  environment, which stays ready while idle and retires after the idle timeout;
- `per_profile_always_running` also shares one environment, and never idles out;
- an acquire request that names another mode than the assignment's is rejected
  and creates nothing.
"""

import secrets
import time

import pytest

from tests.personal_hermes import native_e2e as scenarios
from tests.support.people import person, ready

pytestmark = [pytest.mark.e2e, pytest.mark.cluster, pytest.mark.slow]

IDLE_SECONDS = 60
MODES = ("per_session", "per_profile_on_demand", "per_profile_always_running")

OPEN_AND_RELEASE = (
    scenarios.PLUGIN_PRELUDE
    + """
import urllib.error, uuid
first = environment("", "m1")
first.execute("true")
second = environment("", "m2")
second.execute("true")
first.cleanup()
second.cleanup()
settings = managed_execution.Provider(Context("")).settings()
client = managed_execution.Client(settings)
other = "per_session" if settings["mode"] != "per_session" else "per_profile_on_demand"
try:
    client.call(
        "/v1/leases/acquire",
        {"session": "m3", "mode": other, "request": uuid.uuid4().hex},
        timeout=30,
    )
    wrong = "granted"
except urllib.error.HTTPError as error:
    wrong = error.code
print(json.dumps({"mode": settings["mode"], "wrong": wrong}))
"""
)

OPEN_ANOTHER = (
    scenarios.PLUGIN_PRELUDE
    + """
later = environment("", "m4")
later.execute("true")
later.cleanup()
print(json.dumps({"ok": True}))
"""
)


def view(platform, receipt):
    return [
        {key: item[key] for key in ("id", "session", "state", "leases", "mode")}
        for item in scenarios.environments(platform.admin, receipt["instanceId"])
    ]


def until(condition, timeout, message):
    scenarios.wait(condition, timeout, message, interval=3)


def test_each_lifetime_mode_keys_shares_and_retires_environments_as_specified(
    platform,
):
    admin, tag = platform.admin, secrets.token_hex(3)
    backend = admin.publish(
        "terminal-backends",
        f"modes-{tag}",
        {
            **platform.state.worker,
            "limits": {
                "idleSeconds": IDLE_SECONDS,
                "maxLeaseSeconds": 900,
                "maxEnvironments": 4,
            },
            "mounts": {"workspace": {"mountPath": "/workspace"}},
        },
    )
    people = {
        mode: person(
            platform,
            "mode",
            {
                "template": {"id": "personal", "version": platform.state.template},
                "terminal": {
                    "default": f"modes-{tag}",
                    "allowed": {f"modes-{tag}": backend},
                    "mode": mode,
                },
            },
        )
        for mode in MODES
    }
    spaces = {mode: ready(platform, receipt) for mode, receipt in people.items()}

    for mode in MODES:
        result = scenarios.hermes_python(
            platform.identifier, spaces[mode], OPEN_AND_RELEASE
        )
        assert result["mode"] == mode, "the managed configuration carries the mode"
        assert result["wrong"] not in ("granted", None), (
            f"{mode}: a request for another mode was granted"
        )
        assert isinstance(result["wrong"], int) and 400 <= result["wrong"] < 500

    # Keyed by session, or shared by the profile.
    until(
        lambda: (
            {e["session"]: e["state"] for e in view(platform, people[MODES[0]])}
            == {"m1": "retired", "m2": "retired"}
        ),
        60,
        "per_session environments were not retired when their sessions ended",
    )
    shared = {}
    for mode in MODES[1:]:
        environments = view(platform, people[mode])
        assert len(environments) == 1, f"{mode}: {environments}"
        assert environments[0]["mode"] == mode
        assert (environments[0]["state"], environments[0]["leases"]) == ("ready", 0)
        assert environments[0]["session"] is None
        shared[mode] = environments[0]["id"]
    for mode in MODES:
        assert not [e for e in view(platform, people[mode]) if e["session"] == "m3"], (
            f"{mode}: a rejected request created an environment"
        )

    # A later session reuses the shared environment; a per-session one starts
    # afresh.
    for mode in MODES:
        scenarios.hermes_python(platform.identifier, spaces[mode], OPEN_ANOTHER)
    for mode in MODES[1:]:
        environments = view(platform, people[mode])
        assert [e["id"] for e in environments] == [shared[mode]], (
            f"{mode}: a new session did not reuse the profile's environment"
        )
    sessions = {e["session"] for e in view(platform, people[MODES[0]])}
    assert sessions == {"m1", "m2", "m4"}, sessions

    # Idle: on demand retires after the timeout, always running does not.
    idle_since = time.monotonic()
    until(
        lambda: (
            view(platform, people["per_profile_on_demand"])[0]["state"] == "retired"
        ),
        IDLE_SECONDS + 120,
        "the on-demand environment never idled out",
    )
    assert time.monotonic() - idle_since >= IDLE_SECONDS - 30, (
        "the on-demand environment retired well before its idle timeout"
    )
    time.sleep(30)
    always = view(platform, people["per_profile_always_running"])
    assert [(e["id"], e["state"]) for e in always] == [
        (shared["per_profile_always_running"], "ready")
    ], f"the always-running environment did not stay: {always}"
    # And it still serves a session at once.
    scenarios.hermes_python(
        platform.identifier, spaces["per_profile_always_running"], OPEN_ANOTHER
    )
    assert [e["id"] for e in view(platform, people["per_profile_always_running"])] == [
        shared["per_profile_always_running"]
    ]
