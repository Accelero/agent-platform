"""Leases and environments as the plugin in a running Hermes uses them (R9, R12).

The plugin's own client (`managed_execution.Client`) talks to the execution
interface from inside the owner's Hermes, so every call is authenticated with
the instance's real credential and answered by real workers:

- a delegate joins its parent's environment with a second lease; a lease of
  another session's environment, a stranger's and a released parent's join are
  refused (`403`), and releasing the parent leaves the child live;
- one instance serves several profiles: the default and an explicit selection
  each reach their backend, profile-scoped environments are shared per backend,
  and a runtime outside the policy is refused;
- a change of the default affects new requests only: live leases keep their
  environments, and new requests get an environment of the new generation;
- a lease never outlives its backend's `maxLeaseSeconds`, however it is renewed;
- an instance at its environment limit is refused alone: another instance with
  the same backend is not crowded out.
"""

import secrets
import time

import pytest

from tests.personal_hermes import native_e2e as scenarios
from tests.support.people import person, ready

pytestmark = [pytest.mark.e2e, pytest.mark.cluster, pytest.mark.slow]

PRELUDE = (
    scenarios.PLUGIN_PRELUDE
    + """
import urllib.error, uuid

def client_for(runtime_id=""):
    return managed_execution.Client(
        managed_execution.Provider(Context(runtime_id)).settings())

def attempt(call):
    try:
        return ["ok", call()]
    except urllib.error.HTTPError as error:
        return ["http", error.code]
    except Exception as error:
        return ["error", str(error)]
"""
)


def backend_definition(platform, **limits):
    return {
        **platform.state.worker,
        "limits": {
            "idleSeconds": 60,
            "maxLeaseSeconds": 900,
            "maxEnvironments": 4,
            **limits,
        },
        "mounts": {"workspace": {"mountPath": "/workspace"}},
    }


def instance_with(platform, backends, default, mode):
    """A new person whose assignment allows `backends` (id -> definition), and
    the names of the backends it was given."""
    tag = secrets.token_hex(3)
    versions = {
        name: platform.admin.publish("terminal-backends", f"{name}-{tag}", definition)
        for name, definition in backends.items()
    }
    names = {name: f"{name}-{tag}" for name in backends}
    receipt = person(
        platform,
        "lease",
        {
            "template": {"id": "personal", "version": platform.state.template},
            "terminal": {
                "default": names[default],
                "allowed": {names[name]: versions[name] for name in backends},
                "mode": mode,
            },
        },
    )
    return receipt, ready(platform, receipt), names


def run(platform, namespace, body):
    return scenarios.hermes_python(
        platform.identifier, namespace, PRELUDE + body, timeout=400
    )


def environments(platform, receipt):
    return scenarios.environments(platform.admin, receipt["instanceId"])


def leases(platform, receipt):
    """Live leases by session (or by backend, for profile environments)."""
    return {
        (e["session"] or e["backend"]["id"], e["id"]): e["leases"]
        for e in environments(platform, receipt)
    }


def test_a_delegate_joins_its_parents_environment_and_no_one_joins_a_stranger(
    platform,
):
    receipt, namespace, names = instance_with(
        platform, {"join": backend_definition(platform)}, "join", "per_session"
    )
    first = run(
        platform,
        namespace,
        """
client = client_for()
parent = client.acquire("pj-a")
child = client.acquire("pj-a", parent["lease"])
other = client.acquire("pj-b")
print(json.dumps({
    "parent": parent["lease"],
    "child": child["lease"],
    "same": child["environment"] == parent["environment"],
    "other": other["environment"] != parent["environment"],
    "cross": attempt(lambda: client.acquire("pj-b", parent["lease"])),
    "stranger": attempt(lambda: client.acquire("pj-a", str(uuid.uuid4()))),
}))
""",
    )
    assert first["same"], "a delegate did not join its parent's environment"
    assert first["child"] != first["parent"], "the join shares the parent's lease"
    assert first["other"], "another session shares the environment"
    for refused in ("cross", "stranger"):
        assert first[refused][0] == "error" and "(403)" in first[refused][1], (
            refused,
            first[refused],
        )
    held = {session: n for (session, _), n in leases(platform, receipt).items()}
    assert held == {"pj-a": 2, "pj-b": 1}, "a refused join created a lease"

    second = run(
        platform,
        namespace,
        f"""
client = client_for()
parent, child = "{first["parent"]}", "{first["child"]}"
print(json.dumps({{
    "release": attempt(lambda: client.call("/v1/leases/release", {{"lease": parent}})),
    "late": attempt(lambda: client.acquire("pj-a", parent)),
    "child": attempt(lambda: client.call("/v1/leases/heartbeat", {{"lease": child}})),
    "parent": attempt(lambda: client.call("/v1/leases/heartbeat", {{"lease": parent}})),
}}))
""",
    )
    assert second["release"] == ["ok", {"released": True}]
    assert second["late"][0] == "error" and "(403)" in second["late"][1], (
        "a released parent admitted a join"
    )
    assert second["child"][0] == "ok", "releasing the parent ended the child's lease"
    assert second["parent"] == ["http", 409], "a released lease was renewed"
    held = {session: n for (session, _), n in leases(platform, receipt).items()}
    assert held == {"pj-a": 1, "pj-b": 1}


def test_profiles_select_their_backends_and_a_new_default_applies_to_new_requests_only(
    platform,
):
    receipt, namespace, names = instance_with(
        platform,
        {"alpha": backend_definition(platform), "beta": backend_definition(platform)},
        "alpha",
        "per_profile_on_demand",
    )
    alpha, beta = names["alpha"], names["beta"]
    first = run(
        platform,
        namespace,
        f"""
default = client_for()
explicit_a, explicit_b = client_for("{alpha}"), client_for("{beta}")
a1, b1 = default.acquire("s1"), explicit_b.acquire("s2")
a2, a3 = explicit_a.acquire("s3"), default.acquire("s4")
b2 = explicit_b.acquire("s5")
print(json.dumps({{
    "alpha": [a1["environment"], a2["environment"], a3["environment"]],
    "beta": [b1["environment"], b2["environment"]],
    "held": [a1["lease"], b1["lease"]],
    "unlisted": attempt(lambda: client_for("outside-{alpha}").acquire("s6")),
}}))
""",
    )
    assert len(set(first["alpha"])) == 1, "the default and an explicit pick differ"
    assert len(set(first["beta"])) == 1, "a profile's sessions got separate workers"
    assert first["alpha"][0] != first["beta"][0], "two backends share a worker"
    outside = first["unlisted"]
    assert outside[0] == "error" and "(403)" in outside[1], outside

    by_backend = {
        e["backend"]["id"]: (e["id"], e["leases"])
        for e in environments(platform, receipt)
    }
    assert by_backend == {
        alpha: (first["alpha"][0], 3),
        beta: (first["beta"][0], 2),
    }, "a refused runtime created an environment, or leases were miscounted"

    # Changing the default is a change of the assignment's backends: new
    # requests reach the new generation, and live leases are left alone.
    path = f"/api/v1/assignments/{receipt['assignmentId']}"
    current = platform.admin.call("GET", path)
    terminal = {**current.json()["spec"]["terminal"], "default": beta}
    platform.admin.call(
        "PATCH",
        path,
        {"terminal": terminal},
        etag=current.headers["ETag"],
        expected=(202,),
    )
    namespace = ready(platform, receipt)
    after = run(
        platform,
        namespace,
        f"""
client = client_for()
old = "{first["held"][0]}"
fresh = client.acquire("s7")
print(json.dumps({{
    "fresh": fresh["environment"],
    "old": attempt(lambda: client.call("/v1/leases/heartbeat", {{"lease": old}})),
}}))
""",
    )
    listed = {e["id"]: e for e in environments(platform, receipt)}
    assert after["fresh"] not in (first["alpha"][0], first["beta"][0]), (
        "a new request joined an environment of the old generation"
    )
    assert listed[after["fresh"]]["backend"]["id"] == beta, (
        "the new default was ignored"
    )
    assert after["old"][0] == "ok", "a change of default ended a live lease"
    assert (
        listed[first["alpha"][0]]["state"],
        listed[first["alpha"][0]]["leases"],
    ) == (
        "ready",
        3,
    )
    assert (listed[first["beta"][0]]["state"], listed[first["beta"][0]]["leases"]) == (
        "ready",
        2,
    )


def test_a_lease_never_outlives_its_backends_lease_limit_however_it_is_renewed(
    platform,
):
    limit = 60
    receipt, namespace, names = instance_with(
        platform,
        {"cap": backend_definition(platform, maxLeaseSeconds=limit)},
        "cap",
        "per_profile_on_demand",
    )
    result = run(
        platform,
        namespace,
        f"""
client = client_for()
lease = client.acquire("cap-1")
held = lease["lease"]
start = time.monotonic()
beats = []
while time.monotonic() - start < {2 * limit}:
    reply = attempt(lambda: client.call("/v1/leases/heartbeat", {{"lease": held}}))
    beats.append([round(time.monotonic() - start), reply])
    if reply[0] != "ok":
        break
    time.sleep(5)
print(json.dumps({{"granted": lease["ttl"], "beats": beats}}))
""",
    )
    assert result["granted"] <= limit, "granted for longer than the backend allows"
    renewed = [
        (at, reply[1]["ttl"]) for at, reply in result["beats"] if reply[0] == "ok"
    ]
    assert renewed, "the lease could not be renewed at all"
    assert all(at + ttl <= limit + 5 for at, ttl in renewed), (
        f"a renewal ran past the limit: {renewed}"
    )
    at, refusal = result["beats"][-1]
    assert refusal == ["http", 409], f"the holder was not cut off: {result['beats']}"
    assert at <= limit, f"the lease outlived its {limit} s limit ({at} s)"
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if sum(leases(platform, receipt).values()) == 0:
            break
        time.sleep(3)
    assert sum(leases(platform, receipt).values()) == 0, (
        "the ended lease still counts against its environment"
    )


def test_an_instance_at_its_environment_limit_does_not_crowd_out_another(platform):
    single = backend_definition(platform, maxEnvironments=1)
    owners = [
        instance_with(platform, {"one": single}, "one", "per_session") for _ in range(2)
    ]
    script = """
client = client_for()
first = client.acquire("{session}-1")
settings = managed_execution.Provider(Context("")).settings()
second = attempt(lambda: client.call(
    "/v1/leases/acquire",
    {{"session": "{session}-2", "mode": settings["mode"], "request": uuid.uuid4().hex}},
    timeout=30))
print(json.dumps({{"first": first["environment"], "second": second}}))
"""
    answers = [
        run(platform, namespace, script.format(session=f"owner{index}"))
        for index, (_, namespace, _) in enumerate(owners)
    ]
    for answer in answers:
        assert answer["second"] == ["http", 503], answer
    assert answers[0]["first"] != answers[1]["first"]
    for receipt, _, _ in owners:
        (only,) = environments(platform, receipt)
        assert only["leases"] == 1, (
            "a refused acquisition left a lease or an environment"
        )
