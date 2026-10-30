"""Controller failover and absence on a platform of its own (R16, R17).

The chart runs one controller; the election supports more. With two candidates
the leader is killed: a standby must take the Lease and commission new work,
and no existing instance may be restarted, recreated or given new credentials.
With no controller at all, healthy Hermes instances keep running untouched.
"""

import time

import pytest

from tests.personal_hermes import native_e2e as scenarios
from tests.support.people import person

pytestmark = [
    pytest.mark.e2e,
    pytest.mark.cluster,
    pytest.mark.destructive,
    pytest.mark.slow,
]

CONTROLLER = "hermes-manager-controller"


def holder(platform):
    return scenarios.kubectl(
        platform.identifier,
        "get",
        "leases",
        "--namespace",
        scenarios.NAMESPACE,
        "-o",
        "jsonpath={.items[*].spec.holderIdentity}",
    ).strip()


def owned_namespaces(platform):
    """The namespaces of every instance this installation owns."""
    return scenarios.kubectl(
        platform.identifier,
        "get",
        "namespaces",
        "--selector",
        f"agent-platform.io/installation={platform.owned['id']}",
        "-o",
        "name",
    ).split()


def identities(platform, namespaces):
    """What must survive any change of controller: the Hermes Pods and the
    objects whose identity would change if something were recreated."""
    state = {}
    for name, namespace in namespaces.items():
        uids = {"hermes": scenarios.hermes_pod(platform.identifier, namespace)}
        for kind, resource in (
            ("namespace", f"namespace/{namespace}"),
            ("credentials", f"secret/hermes-credentials -n {namespace}"),
            ("worker keys", f"secret/hermes-worker-keys -n {namespace}"),
        ):
            uids[kind] = scenarios.kubectl(
                platform.identifier,
                "get",
                *resource.split(),
                "-o",
                "jsonpath={.metadata.uid}",
            )
        state[name] = uids
    return state


def test_a_standby_takes_over_a_killed_leader_and_no_instance_is_disturbed(
    destructive_platform,
):
    platform = destructive_platform
    admin = platform.admin
    namespaces = {
        name: admin.phase(receipt["instanceId"])["namespace"]
        for name, receipt in platform.assignments.items()
    }
    before = identities(platform, namespaces)
    owned_before = owned_namespaces(platform)

    scenarios.scale(platform.identifier, scenarios.NAMESPACE, CONTROLLER, 2)
    leader = holder(platform)
    assert leader, "a leader holds the Lease"
    scenarios.kubectl(
        platform.identifier,
        "delete",
        "pod",
        # The holder identity is the Pod name plus a per-process suffix.
        leader.rsplit("-", 1)[0],
        "--namespace",
        scenarios.NAMESPACE,
        "--grace-period=0",
        "--force",
    )
    scenarios.wait(
        lambda: holder(platform) not in ("", leader),
        120,
        "no standby took over the Lease",
        interval=2,
    )

    # The new leader acts: new work is commissioned while the old one is gone.
    receipt = person(
        platform,
        "standby",
        {
            "template": {"id": "personal", "version": platform.state.template},
            "volumes": {"workspace": {"source": "local", "sizeGib": 1}},
            "terminal": platform.state.terminal,
        },
    )
    scenarios.wait(
        lambda: admin.phase(receipt["instanceId"])["phase"] == "ready",
        scenarios.READY_TIMEOUT,
        "the new leader never commissioned the new instance",
        interval=5,
    )

    assert identities(platform, namespaces) == before, "an instance was disturbed"
    owned = owned_namespaces(platform)
    assert len(owned) == len(owned_before) + 1, f"one namespace per instance: {owned}"

    # The platform is shared: back to the single controller the chart installs.
    scenarios.scale(platform.identifier, scenarios.NAMESPACE, CONTROLLER, 1)
    scenarios.wait(
        lambda: holder(platform) != "",
        120,
        "no controller holds the Lease again",
        interval=2,
    )


def test_instances_keep_running_untouched_while_there_is_no_controller(
    destructive_platform,
):
    platform = destructive_platform
    admin = platform.admin
    namespaces = {
        name: admin.phase(receipt["instanceId"])["namespace"]
        for name, receipt in platform.assignments.items()
    }
    before = identities(platform, namespaces)

    scenarios.scale(platform.identifier, scenarios.NAMESPACE, CONTROLLER, 0)
    try:
        # Long enough for any restart a missing controller could cause.
        time.sleep(60)
        assert identities(platform, namespaces) == before
        for namespace in namespaces.values():
            ready = scenarios.kubectl(
                platform.identifier,
                "get",
                "deployment/hermes",
                "--namespace",
                namespace,
                "-o",
                "jsonpath={.status.readyReplicas}",
            )
            assert ready == "1", f"Hermes stopped without the controller: {ready!r}"
    finally:
        scenarios.scale(platform.identifier, scenarios.NAMESPACE, CONTROLLER, 1)

    scenarios.wait(
        lambda: all(
            admin.phase(receipt["instanceId"])["phase"] == "ready"
            for receipt in platform.assignments.values()
        ),
        600,
        "the instances did not stay ready after the controller returned",
        interval=5,
    )
    assert identities(platform, namespaces) == before, "the return disturbed them"
