"""A worker that dies mid-command (R9, R12, R14, R17).

The specification: worker loss never deletes volume data, an interrupted
command is not replayed, and a failed worker drains and is never replaced by
another one. The case starts a long command in a real worker, stops the
worker's container on the node, and checks all three, then that a new session
gets a fresh worker of the same backend on the same data.
"""

import secrets
import subprocess
import time

import pytest

from tests.personal_hermes import native_e2e as scenarios
from tests.support.people import person, ready

pytestmark = [pytest.mark.e2e, pytest.mark.cluster, pytest.mark.slow]

RUNTIME = "--runtime-endpoint unix:///run/k3s/containerd/containerd.sock"
# Leases that nobody renews expire after two minutes; draining waits for them.
RETIRED_WITHIN = 330

LONG_COMMAND = (
    scenarios.PLUGIN_PRELUDE
    + """
shell = environment("", "k1")
print("started", flush=True)
try:
    reply = shell.execute(
        "echo started >> /workspace/loss-log; sleep 240; "
        "echo finished >> /workspace/loss-log"
    )
    print(json.dumps({"returned": reply}), flush=True)
except Exception as error:
    print(json.dumps({"raised": str(error)[:300]}), flush=True)
"""
)

NEXT_SESSION = (
    scenarios.PLUGIN_PRELUDE
    + """
shell = environment("", "k2")
reply = shell.execute("cat /workspace/loss-log; id -u")
shell.cleanup()
print(json.dumps({"output": reply["output"], "returncode": reply["returncode"]}))
"""
)


def worker_pods(platform, namespace):
    pods = scenarios.kubectl(
        platform.identifier, "get", "pods", "--namespace", namespace, "-o", "name"
    ).split()
    return {pod.split("/", 1)[1] for pod in pods if pod.startswith("pod/hw-")}


def stop_worker(platform, namespace):
    """Stops the worker's container through the node's runtime, as a crash of
    the container would."""
    listed = subprocess.run(
        [
            "docker",
            "exec",
            platform.identifier,
            "sh",
            "-c",
            f"crictl {RUNTIME} ps --label io.kubernetes.pod.namespace={namespace} "
            "--name execution -q",
        ],
        capture_output=True,
        text=True,
        timeout=60,
        check=True,
    ).stdout.split()
    assert len(listed) == 1, f"workers of the instance: {listed}"
    subprocess.run(
        [
            "docker",
            "exec",
            platform.identifier,
            "sh",
            "-c",
            f"crictl {RUNTIME} stop -t 0 {listed[0]}",
        ],
        capture_output=True,
        timeout=60,
        check=True,
    )


def test_a_worker_lost_mid_command_is_not_replayed_or_replaced_and_keeps_its_data(
    platform,
):
    admin, tag = platform.admin, secrets.token_hex(3)
    backend = admin.publish(
        "terminal-backends",
        f"loss-{tag}",
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
        "loss",
        {
            "template": {"id": "personal", "version": platform.state.template},
            "terminal": {
                "default": f"loss-{tag}",
                "allowed": {f"loss-{tag}": backend},
            },
        },
    )
    namespace = ready(platform, receipt)
    instance = receipt["instanceId"]
    hermes = scenarios.hermes_pod(platform.identifier, namespace)

    process = scenarios.hermes_python_started(
        platform.identifier, namespace, LONG_COMMAND
    )
    try:
        assert process.stdout.readline().strip() == "started"
        scenarios.wait(
            lambda: (
                scenarios.instance_pod_exec(
                    platform.identifier,
                    namespace,
                    "cat /workspace/loss-log 2>/dev/null",
                ).split()
                == ["started"]
            ),
            120,
            "the command never started in the worker",
            interval=1,
        )
        original = worker_pods(platform, namespace)
        assert len(original) == 1, original
        lost = scenarios.environments(admin, instance)
        assert [(e["state"], e["leases"]) for e in lost] == [("ready", 1)]

        stop_worker(platform, namespace)
        output, _ = process.communicate(timeout=120)
    finally:
        if process.poll() is None:
            process.kill()
    # Interrupted: the call fails; nothing finished it, and nothing reran it.
    assert '"returncode": 255' in output or '"raised"' in output, output
    assert "finished" not in output

    seen, states = set(original), []
    deadline = time.monotonic() + RETIRED_WITHIN
    while True:
        environment = {e["id"]: e for e in scenarios.environments(admin, instance)}[
            lost[0]["id"]
        ]
        states.append(environment["state"])
        seen |= worker_pods(platform, namespace)
        if environment["state"] == "retired":
            break
        assert time.monotonic() < deadline, f"never retired: {states[-5:]}"
        time.sleep(5)
    left = next(i for i, state in enumerate(states) if state != "ready")
    assert "ready" not in states[left:], (
        f"the lost worker's environment became ready again: {states}"
    )
    assert seen == original, f"another worker replaced it: {seen - original}"
    assert not worker_pods(platform, namespace), "the lost worker's Pod was left"
    log = scenarios.instance_pod_exec(
        platform.identifier, namespace, "cat /workspace/loss-log"
    )
    assert log.split() == ["started"], f"the command was replayed or finished: {log}"
    assert scenarios.hermes_pod(platform.identifier, namespace) == hermes, (
        "Hermes was restarted by the loss of a worker"
    )

    # The next session gets its own worker of the same backend, on the same data.
    result = scenarios.hermes_python(platform.identifier, namespace, NEXT_SESSION)
    assert result["returncode"] == 0
    assert result["output"].split() == ["started", "1000"], result
    after = {e["id"]: e for e in scenarios.environments(admin, instance)}
    assert len(after) == 2 and lost[0]["id"] in after
    fresh = next(e for i, e in after.items() if i != lost[0]["id"])
    assert fresh["backend"] == lost[0]["backend"]
    assert fresh["failure"] is None
