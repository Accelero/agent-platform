"""Live NetworkPolicy allow/deny matrix between the platform's roles and owners.

The policies under test are the ones the platform installs: the chart's
control-plane NetworkPolicies, and the renderer's instance policies configured
as the chart configures the controller. Pods carry the labels of the chart's
own Deployments. The expectations come from the specification's network list:

- the gateway reaches the native Hermes port;
- the controller reaches the native port and worker SSH for readiness probes;
- Hermes reaches the manager execution API, its own workers' SSH and DNS;
- workers accept SSH only from their own Hermes and the controller;
- everything else is denied.

Every denied path is compared with the same path before any policy existed (so
the policy is the cause) and with a live listener (so a dead server is not an
isolation oracle). The matrix runs with all roles on one node and spread over
three, because policy enforcement is per node.
"""

from __future__ import annotations

import json
import uuid
from dataclasses import dataclass, field

import pytest

from tests.support import cluster as k
from tests.support.chart import Chart

pytestmark = [
    pytest.mark.e2e,
    pytest.mark.cluster,
    pytest.mark.multi_node,
    pytest.mark.security,
]

NATIVE = 9119
SSH = 2222
EXTRA_NATIVE = 9998


@dataclass
class Topology:
    cluster: k.Cluster
    chart: Chart
    placement: str
    spaces: dict
    pods: dict
    ips: dict = field(default_factory=dict)
    baseline: dict = field(default_factory=dict)

    def connects(self, source, destination, port):
        space, name = self.pods[source]
        return k.connects(self.cluster, name, space, self.ips[destination], port)

    def alive(self, destination, port):
        """The listener answers on its own loopback, whatever the policies say."""
        space, name = self.pods[destination]
        return k.connects(
            self.cluster,
            name,
            space,
            "127.0.0.1",
            port,
            container=f"listen-{port}",
        )

    def resolves(self, source):
        space, name = self.pods[source]
        result = self.cluster.kubectl(
            "exec",
            "-n",
            space,
            name,
            "-c",
            k.first_container(self.cluster, name, space),
            "--",
            "nslookup",
            "kubernetes.default.svc.cluster.local",
            check=False,
            timeout=30,
        )
        return result.returncode == 0


def layout(cluster, chart, suffix):
    installation = {"agent-platform.io/installation": chart.installation}
    return {
        "control": (chart.namespace, {}),
        "ingress": ("ingress", {}),
        "a": (f"hi-a-{suffix}", installation),
        "b": (f"hi-b-{suffix}", installation),
        "elsewhere": (f"elsewhere-{suffix}", {}),
    }


def nodes(cluster, placement):
    server, first, second = cluster.servers[0], cluster.agents[0], cluster.agents[1]
    if placement == "same-node":
        return dict.fromkeys(
            (
                "gateway",
                "controller",
                "api",
                "proxy",
                "hermes-a",
                "worker-a",
                "stranger-a",
                "hermes-b",
                "worker-b",
                "intruder",
                "spoof-gateway",
                "spoof-controller",
                "spoof-hermes",
            ),
            first,
        )
    return {
        "gateway": server,
        "controller": server,
        "api": second,
        "proxy": first,
        "hermes-a": first,
        "worker-a": second,
        "stranger-a": first,
        "hermes-b": second,
        "worker-b": first,
        "intruder": second,
        "spoof-gateway": second,
        "spoof-controller": first,
        "spoof-hermes": second,
    }


def build(cluster, chart, placement):
    suffix = uuid.uuid4().hex[:6]
    spaces = layout(cluster, chart, suffix)
    where = nodes(cluster, placement)
    image = cluster.probe_image

    def pod(key, space, name, labels, ports):
        return (
            key,
            space,
            name,
            k.probe_pod(
                name,
                spaces[space][0],
                image,
                labels=labels,
                ports=ports,
                node=where[key],
            ),
        )

    component = "app.kubernetes.io/component"
    specs = [
        pod(
            "gateway",
            "control",
            "gateway",
            chart.pod_labels("hermes-gateway"),
            chart.container_ports("hermes-gateway"),
        ),
        pod(
            "controller",
            "control",
            "controller",
            chart.pod_labels("hermes-manager-controller"),
            [9000],
        ),
        pod(
            "api",
            "control",
            "api",
            chart.pod_labels("hermes-manager-api"),
            chart.container_ports("hermes-manager-api"),
        ),
        pod("proxy", "ingress", "proxy", {"app": "ingress"}, [9100]),
        pod("hermes-a", "a", "hermes", {component: "hermes"}, [NATIVE, EXTRA_NATIVE]),
        pod("worker-a", "a", "worker", {component: "worker"}, [SSH]),
        pod("stranger-a", "a", "stranger", {component: "other"}, [7000]),
        pod("hermes-b", "b", "hermes", {component: "hermes"}, [NATIVE]),
        pod("worker-b", "b", "worker", {component: "worker"}, [SSH]),
        pod("intruder", "elsewhere", "intruder", {"app": "intruder"}, [7001]),
        # Pods in an unrelated namespace that carry a platform role's labels.
        pod(
            "spoof-gateway",
            "elsewhere",
            "spoof-gateway",
            chart.pod_labels("hermes-gateway"),
            [7101],
        ),
        pod(
            "spoof-controller",
            "elsewhere",
            "spoof-controller",
            chart.pod_labels("hermes-manager-controller"),
            [7102],
        ),
        pod(
            "spoof-hermes",
            "elsewhere",
            "spoof-hermes",
            {component: "hermes"},
            [7103],
        ),
    ]
    return spaces, specs


def delete_spaces(cluster, spaces):
    for name, _ in spaces.values():
        cluster.kubectl(
            "delete",
            "namespace",
            name,
            "--wait=true",
            "--timeout=120s",
            check=False,
            timeout=150,
        )


@pytest.fixture(scope="module", params=["same-node", "across-nodes"])
def topology(request, cluster):
    chart = Chart()
    spaces, specs = build(cluster, chart, request.param)
    try:
        cluster.apply([k.namespace(name, labels) for name, labels in spaces.values()])
        cluster.apply([spec for *_, spec in specs])
        pods = {key: (spaces[space][0], name) for key, space, name, _ in specs}
        k.wait_ready(cluster, list(pods.values()))
        built = Topology(cluster, chart, request.param, spaces, pods)
        built.ips = {
            key: k.pod_ip(cluster, name, space) for key, (space, name) in pods.items()
        }
        # The same paths before any policy: every one of them must connect.
        for row in DENIED:
            built.baseline[row.id] = built.connects(
                row.source, row.destination, row.port
            )
        # Names resolve for an unrestricted pod once the DNS server answers.
        try:
            cluster.wait_for(
                lambda: built.resolves("stranger-a"), 60, "cluster DNS", interval=2
            )
            built.baseline["dns:stranger-a"] = True
        except k.ClusterUnavailable:
            built.baseline["dns:stranger-a"] = False
        # The platform's own policies, exactly as installed.
        cluster.apply(chart.policies())
        for key in ("a", "b"):
            cluster.apply(chart.instance_policies(spaces[key][0]))
        # Enforcement barrier: a known-denied path closes and a known-allowed
        # path stays open, twice in a row, before anything is asserted.
        settled = []

        def enforced():
            denied = not built.connects("hermes-b", "worker-a", SSH)
            allowed = built.connects("hermes-a", "worker-a", SSH)
            settled.append(denied and allowed)
            return len(settled) >= 2 and all(settled[-2:])

        cluster.wait_for(enforced, 90, "the policies to be enforced", interval=2)
        yield built
    finally:
        delete_spaces(cluster, spaces)


@dataclass(frozen=True)
class Path:
    id: str
    source: str
    destination: str
    port: int


ALLOWED = [
    Path(
        "gateway-reaches-the-native-port-of-its-instance", "gateway", "hermes-a", NATIVE
    ),
    Path(
        "gateway-reaches-the-native-port-of-the-other-instance",
        "gateway",
        "hermes-b",
        NATIVE,
    ),
    Path("controller-probes-the-native-port", "controller", "hermes-a", NATIVE),
    Path("controller-probes-worker-ssh", "controller", "worker-a", SSH),
    Path(
        "controller-probes-worker-ssh-of-the-other-instance",
        "controller",
        "worker-b",
        SSH,
    ),
    Path("hermes-reaches-its-own-worker-ssh", "hermes-a", "worker-a", SSH),
    Path("the-other-hermes-reaches-its-own-worker-ssh", "hermes-b", "worker-b", SSH),
    Path("hermes-reaches-the-execution-api", "hermes-a", "api", 8445),
    Path("the-other-hermes-reaches-the-execution-api", "hermes-b", "api", 8445),
    Path("the-gateway-reaches-the-resolver-listener", "gateway", "api", 8444),
    Path("the-ingress-reaches-the-administration-listener", "proxy", "api", 8443),
]
DENIED = [
    Path(
        "another-owners-hermes-cannot-reach-this-workers-ssh",
        "hermes-b",
        "worker-a",
        SSH,
    ),
    Path(
        "another-owners-worker-cannot-reach-this-workers-ssh",
        "worker-b",
        "worker-a",
        SSH,
    ),
    Path(
        "another-owners-hermes-cannot-reach-this-native-port",
        "hermes-b",
        "hermes-a",
        NATIVE,
    ),
    Path(
        "this-hermes-cannot-reach-another-owners-native-port",
        "hermes-a",
        "hermes-b",
        NATIVE,
    ),
    Path(
        "this-hermes-cannot-reach-another-owners-worker-ssh",
        "hermes-a",
        "worker-b",
        SSH,
    ),
    Path(
        "an-unrelated-namespace-cannot-reach-the-native-port",
        "intruder",
        "hermes-a",
        NATIVE,
    ),
    Path("an-unrelated-namespace-cannot-reach-worker-ssh", "intruder", "worker-a", SSH),
    Path("a-worker-cannot-reach-the-manager-api", "worker-a", "api", 8445),
    Path("a-worker-cannot-reach-its-own-hermes", "worker-a", "hermes-a", NATIVE),
    Path(
        "another-pod-in-the-namespace-cannot-reach-worker-ssh",
        "stranger-a",
        "worker-a",
        SSH,
    ),
    Path(
        "another-pod-in-the-namespace-cannot-reach-the-native-port",
        "stranger-a",
        "hermes-a",
        NATIVE,
    ),
    Path("the-gateway-cannot-reach-worker-ssh", "gateway", "worker-a", SSH),
    Path("the-manager-api-cannot-reach-the-native-port", "api", "hermes-a", NATIVE),
    Path("hermes-cannot-reach-the-administration-listener", "hermes-a", "api", 8443),
    Path("hermes-cannot-reach-the-resolver-listener", "hermes-a", "api", 8444),
    Path("the-gateway-cannot-reach-the-execution-listener", "gateway", "api", 8445),
    Path("the-ingress-cannot-reach-the-execution-listener", "proxy", "api", 8445),
    Path("hermes-cannot-reach-an-unrelated-namespace", "hermes-a", "intruder", 7001),
    Path(
        "hermes-cannot-reach-another-pod-in-its-namespace",
        "hermes-a",
        "stranger-a",
        7000,
    ),
    Path(
        "the-controller-cannot-reach-an-unspecified-hermes-port",
        "controller",
        "hermes-a",
        EXTRA_NATIVE,
    ),
    Path(
        "the-gateway-cannot-reach-an-unspecified-hermes-port",
        "gateway",
        "hermes-a",
        EXTRA_NATIVE,
    ),
    Path(
        "a-gateway-lookalike-in-another-namespace-cannot-reach-the-native-port",
        "spoof-gateway",
        "hermes-a",
        NATIVE,
    ),
    Path(
        "a-controller-lookalike-in-another-namespace-cannot-reach-worker-ssh",
        "spoof-controller",
        "worker-a",
        SSH,
    ),
    Path(
        "a-controller-lookalike-in-another-namespace-cannot-reach-the-native-port",
        "spoof-controller",
        "hermes-a",
        NATIVE,
    ),
    Path(
        "a-hermes-lookalike-in-another-namespace-cannot-reach-worker-ssh",
        "spoof-hermes",
        "worker-a",
        SSH,
    ),
    Path(
        "a-hermes-lookalike-in-another-namespace-cannot-reach-the-execution-listener",
        "spoof-hermes",
        "api",
        8445,
    ),
    Path("nothing-reaches-the-controller", "gateway", "controller", 9000),
    Path("an-instance-cannot-reach-the-controller", "hermes-a", "controller", 9000),
]


@pytest.mark.parametrize("path", ALLOWED, ids=[path.id for path in ALLOWED])
def test_the_documented_path_works(topology, path):
    assert topology.connects(path.source, path.destination, path.port)


@pytest.mark.parametrize("path", DENIED, ids=[path.id for path in DENIED])
def test_every_other_path_is_denied_by_policy_not_by_a_dead_server(topology, path):
    # The policy is the cause: the same path connected before any policy ...
    assert topology.baseline[path.id], "the path was not open before the policies"
    # ... and the destination is a live listener now.
    assert topology.alive(path.destination, path.port), "the listener is not alive"
    assert not topology.connects(path.source, path.destination, path.port)


def test_dns_is_open_to_hermes_and_workers_only(topology):
    assert topology.baseline["dns:stranger-a"], (
        "names did not resolve before the policies"
    )
    assert topology.resolves("hermes-a")
    assert topology.resolves("worker-a")
    assert not topology.resolves("stranger-a")


def test_removing_or_widening_a_required_policy_changes_the_packet_level_outcome(
    cluster,
):
    """The matrix is sensitive to the policies: it is not a fixed answer.

    The probe that widening must admit is an outsider in a namespace without
    policies. A pod in the instance's own namespace would stay denied by its
    own default-deny egress whatever the worker's ingress said.
    """
    chart = Chart()
    suffix = uuid.uuid4().hex[:6]
    name, outside = f"hi-s-{suffix}", f"outside-{suffix}"
    image = cluster.probe_image
    component = "app.kubernetes.io/component"
    try:
        cluster.apply(
            [
                k.namespace(
                    name, {"agent-platform.io/installation": chart.installation}
                ),
                k.namespace(outside),
                k.probe_pod(
                    "hermes", name, image, labels={component: "hermes"}, ports=[NATIVE]
                ),
                k.probe_pod(
                    "worker", name, image, labels={component: "worker"}, ports=[SSH]
                ),
                k.probe_pod(
                    "outsider", outside, image, labels={"app": "outsider"}, ports=[7002]
                ),
            ]
        )
        k.wait_ready(
            cluster, [(name, "hermes"), (name, "worker"), (outside, "outsider")]
        )
        worker = k.pod_ip(cluster, "worker", name)

        def reaches(source, space):
            return k.connects(cluster, source, space, worker, SSH)

        assert reaches("hermes", name) and reaches("outsider", outside), (
            "open before any policy"
        )
        cluster.apply(chart.instance_policies(name))
        cluster.wait_for(
            lambda: not reaches("outsider", outside), 90, "the policies", interval=2
        )
        assert reaches("hermes", name), "the owner's Hermes keeps its path"

        # Widening the worker's ingress to every namespace admits the outsider.
        cluster.kubectl(
            "patch",
            "networkpolicy",
            "worker",
            "-n",
            name,
            "--type=json",
            "-p",
            json.dumps(
                [
                    {
                        "op": "add",
                        "path": "/spec/ingress/0/from/-",
                        "value": {"namespaceSelector": {}},
                    }
                ]
            ),
        )
        cluster.wait_for(
            lambda: reaches("outsider", outside),
            90,
            "the widened selector",
            interval=2,
        )
        assert reaches("hermes", name)

        # Removing the allow policy leaves default-deny: not even Hermes.
        cluster.kubectl("delete", "networkpolicy", "worker", "-n", name)
        cluster.wait_for(
            lambda: not reaches("hermes", name),
            90,
            "the removed allow",
            interval=2,
        )
        assert not reaches("outsider", outside)

        # Removing default-deny as well opens the worker to everyone again.
        cluster.kubectl("delete", "networkpolicy", "default-deny", "-n", name)
        cluster.wait_for(
            lambda: reaches("hermes", name) and reaches("outsider", outside),
            90,
            "the removed deny",
            interval=2,
        )
    finally:
        for space in (name, outside):
            cluster.kubectl(
                "delete",
                "namespace",
                space,
                "--wait=true",
                "--timeout=120s",
                check=False,
                timeout=150,
            )
