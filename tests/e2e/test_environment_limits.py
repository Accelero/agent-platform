"""A backend's `maxEnvironments` against real workers (R9, R12).

A per-session backend limited to two environments serves two sessions. A third
is refused with `503 dependency_unavailable` at once: no environment record, no
worker Pod, and the two live sessions are untouched. When one of them ends and
its environment retires, the room it held comes back and the third session is
served.
"""

import json
import secrets
import time

import pytest

from tests.personal_hermes import native_e2e as scenarios
from tests.support.people import person, ready

pytestmark = [pytest.mark.e2e, pytest.mark.cluster, pytest.mark.slow]

HOLD = (
    scenarios.PLUGIN_PRELUDE
    + """
import urllib.error, uuid
settings = managed_execution.Provider(Context("")).settings()
client = managed_execution.Client(settings)

def acquire(session):
    try:
        client.call(
            "/v1/leases/acquire",
            {"session": session, "mode": settings["mode"], "request": uuid.uuid4().hex},
            timeout=30,
        )
        return 200
    except urllib.error.HTTPError as error:
        return error.code

first = environment("", "s1")
first.execute("true")
second = environment("", "s2")
second.execute("true")
print("held", flush=True)
print(json.dumps({"third": acquire("s3")}), flush=True)
while not Path("/tmp/go").exists():
    time.sleep(0.5)
first.cleanup()
deadline = time.monotonic() + 150
code = None
while time.monotonic() < deadline:
    code = acquire("s3")
    if code == 200:
        break
    time.sleep(2)
print(json.dumps({"after": code}), flush=True)
second.cleanup()
"""
)


def workers(platform, namespace):
    names = scenarios.kubectl(
        platform.identifier,
        "get",
        "pods",
        "--namespace",
        namespace,
        "--no-headers",
        "-o",
        "custom-columns=N:.metadata.name",
    ).split()
    return [name for name in names if name.startswith("hw-")]


def test_a_backend_serves_no_more_environments_than_it_allows_until_one_retires(
    platform,
):
    admin, tag = platform.admin, secrets.token_hex(3)
    backend = admin.publish(
        "terminal-backends",
        f"cap-{tag}",
        {
            **platform.state.worker,
            "limits": {
                "idleSeconds": 300,
                "maxLeaseSeconds": 900,
                "maxEnvironments": 2,
            },
            "mounts": {"workspace": {"mountPath": "/workspace"}},
        },
    )
    receipt = person(
        platform,
        "cap",
        {
            "template": {"id": "personal", "version": platform.state.template},
            "terminal": {
                "default": f"cap-{tag}",
                "allowed": {f"cap-{tag}": backend},
            },
        },
    )
    namespace = ready(platform, receipt)

    def sessions():
        return {
            e["session"]: (e["state"], e["leases"])
            for e in scenarios.environments(admin, receipt["instanceId"])
        }

    process = scenarios.hermes_python_started(platform.identifier, namespace, HOLD)
    try:
        assert process.stdout.readline().strip() == "held"
        assert json.loads(process.stdout.readline()) == {"third": 503}
        # The refusal created nothing, and the two live sessions are untouched.
        assert sessions() == {"s1": ("ready", 1), "s2": ("ready", 1)}
        assert len(workers(platform, namespace)) == 2
        time.sleep(5)
        assert sessions() == {"s1": ("ready", 1), "s2": ("ready", 1)}

        # One session ends; the room it held comes back.
        scenarios.instance_pod_exec(platform.identifier, namespace, "touch /tmp/go")
        assert json.loads(process.stdout.readline()) == {"after": 200}
        now = sessions()
        assert now["s1"] == ("retired", 0)
        assert now["s2"] == ("ready", 1)
        assert now["s3"][0] == "ready"
        output, _ = process.communicate(timeout=120)
        assert output.strip() == ""
    finally:
        if process.poll() is None:
            process.kill()
