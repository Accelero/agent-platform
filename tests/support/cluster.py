"""One run-owned k3d cluster for every cluster test.

The cluster is created from an explicit, reviewed configuration: a pinned node
image, a loopback-only API port, a private kubeconfig and run labels on every
node container. It neither reads a deployment context nor changes the user's
default kubeconfig. Teardown is registered the moment the cluster exists and
removes exactly the nodes this run labelled.

The k3d binary is named by `HERMES_TEST_K3D`; there is no PATH lookup and
nothing is downloaded. Network policy enforcement is proven by a functional
allow/deny probe before the cluster is handed to a test.
"""

from __future__ import annotations

import json
import os
import socket
import subprocess
import tempfile
import time
import uuid
from dataclasses import dataclass
from pathlib import Path

import yaml

from tests.personal_hermes.kubernetes import IMAGE as NODE_IMAGE
from tests.personal_hermes.kubernetes import import_image
from tests.support.ownership import (
    FIXTURE,
    KUBERNETES,
    RUN,
    Cleanup,
    new_run,
    require_fixture,
)

K3D_VERSION = "v5.9.0"
# k3d's helper container, pinned by digest like every other fixture image.
K3D_TOOLS = (
    "ghcr.io/k3d-io/k3d-tools@sha256:"
    "c57abce697e66feeb826922718738fe186b47779312d49d4e51e6fb9b8a6f34d"
)
# The probe workload: a digest-pinned registry image, which is both a live
# HTTP listener (any port, set by environment) and carries a busybox `wget`.
PROBE_SOURCE = (
    "registry@sha256:ddf754342cfc8acc51a56d5d0ab6af06826461864460636d8bd5c546dab2a7b8"
)
PROBE_TAG = "agent-platform-probe:test"


class ClusterUnavailable(RuntimeError):
    """The cluster fixture could not be provided; this is never a skip."""


def k3d():
    """The explicitly named k3d binary, at the pinned version."""
    path = os.environ.get("HERMES_TEST_K3D")
    if not path:
        raise ClusterUnavailable("HERMES_TEST_K3D names the k3d binary")
    shown = subprocess.run(
        [path, "version"], capture_output=True, text=True, timeout=20
    ).stdout
    if f"k3d version {K3D_VERSION}" not in shown:
        raise ClusterUnavailable(f"k3d {K3D_VERSION} is required")
    return path


def run(args, *, timeout=60, data=None, env=None):
    """Runs a bounded command; a failure names the command, never its output."""
    result = subprocess.run(
        args, capture_output=True, input=data, timeout=timeout, env=env
    )
    if result.returncode != 0:
        raise ClusterUnavailable(f"{Path(args[0]).name} {args[1]} failed")
    return result.stdout.decode()


def free_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def configuration(name, run_id, agents, port):
    return {
        "apiVersion": "k3d.io/v1alpha5",
        "kind": "Simple",
        "metadata": {"name": name},
        "servers": 1,
        "agents": agents,
        "image": NODE_IMAGE,
        "kubeAPI": {"host": "127.0.0.1", "hostIP": "127.0.0.1", "hostPort": str(port)},
        "options": {
            "k3d": {
                "wait": True,
                "timeout": "240s",
                "disableLoadbalancer": True,
            },
            "k3s": {
                "extraArgs": [
                    {
                        "arg": "--disable=traefik,servicelb,metrics-server",
                        "nodeFilters": ["server:*"],
                    }
                ]
            },
            "kubeconfig": {
                "updateDefaultKubeconfig": False,
                "switchCurrentContext": False,
            },
            "runtime": {
                "labels": [
                    {"label": f"{FIXTURE}={KUBERNETES}", "nodeFilters": ["all"]},
                    {"label": f"{RUN}={run_id}", "nodeFilters": ["all"]},
                ]
            },
        },
    }


@dataclass
class Cluster:
    name: str
    run: str
    directory: Path
    servers: list[str]
    agents: list[str]
    probe_image: str = ""

    @property
    def nodes(self):
        return [*self.servers, *self.agents]

    @property
    def kubeconfig(self):
        return self.directory / "kubeconfig"

    def kubectl(self, *args, data=None, timeout=120, check=True):
        """kubectl inside the server node, against this cluster only."""
        result = subprocess.run(
            ["docker", "exec", "-i", self.servers[0], "kubectl", *args],
            input=data,
            capture_output=True,
            timeout=timeout,
        )
        if check and result.returncode != 0:
            raise ClusterUnavailable(f"kubectl {args[0]} failed")
        return result

    def apply(self, documents):
        self.kubectl("apply", "-f", "-", data=yaml.safe_dump_all(documents).encode())

    def get(self, *args):
        return self.kubectl("get", *args, "-o", "json").stdout.decode()

    def wait_for(self, condition, seconds, what, interval=1.0):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if condition():
                return
            time.sleep(interval)
        raise ClusterUnavailable(f"timed out waiting for {what}")


def create(agents: int, directory: Path, cleanup: Cleanup) -> Cluster:
    """A cluster of one server and `agents` agents, removed with `cleanup`."""
    binary = k3d()
    environment = {**os.environ, "K3D_IMAGE_TOOLS": K3D_TOOLS}
    run_id = new_run()
    name = f"hermes-{run_id[:8]}"
    config = directory / "k3d.yaml"
    config.write_text(yaml.safe_dump(configuration(name, run_id, agents, free_port())))
    servers = [f"k3d-{name}-server-0"]
    nodes = [*servers, *(f"k3d-{name}-agent-{n}" for n in range(agents))]

    def remove():
        # Only the nodes this run labelled; anything else is left alone.
        for node in nodes:
            exists = subprocess.run(
                ["docker", "inspect", node], capture_output=True, timeout=30
            )
            if exists.returncode == 0:
                require_fixture(node, KUBERNETES, run_id)
        run([binary, "cluster", "delete", name], timeout=180, env=environment)

    cleanup.add(f"k3d cluster {name}", remove)
    run(
        [binary, "cluster", "create", "--config", str(config)],
        timeout=420,
        env=environment,
    )
    for node in nodes:
        require_fixture(node, KUBERNETES, run_id)
    cluster = Cluster(name, run_id, directory, servers, nodes[1:])
    kubeconfig = run([binary, "kubeconfig", "get", name], env=environment)
    cluster.kubeconfig.write_text(kubeconfig)
    cluster.kubeconfig.chmod(0o600)
    cluster.wait_for(
        lambda: (
            len(json.loads(cluster.get("nodes"))["items"]) == len(nodes)
            and all(
                any(
                    c["type"] == "Ready" and c["status"] == "True"
                    for c in item["status"]["conditions"]
                )
                for item in json.loads(cluster.get("nodes"))["items"]
            )
        ),
        180,
        "every node to be ready",
    )
    # Name resolution is part of the policies under test; its server must be
    # up before any baseline is taken.
    # k3s applies its bundled manifests shortly after the first node is ready.
    cluster.wait_for(
        lambda: (
            cluster.kubectl(
                "get", "deployment", "coredns", "-n", "kube-system", check=False
            ).returncode
            == 0
        ),
        180,
        "the CoreDNS deployment to exist",
        interval=2,
    )
    cluster.kubectl(
        "rollout",
        "status",
        "--namespace",
        "kube-system",
        "deployment/coredns",
        "--timeout=180s",
        timeout=200,
    )
    return cluster


def import_probe_image(cluster: Cluster):
    """Imports the pinned probe image into every node; returns its reference."""
    run(["docker", "tag", PROBE_SOURCE, PROBE_TAG])
    references = set()
    with tempfile.TemporaryDirectory(prefix="probe-image-") as temporary:
        for node in cluster.nodes:
            references.add(import_image(node, PROBE_TAG, temporary))
    if len(references) != 1:
        raise ClusterUnavailable("the probe image differs between nodes")
    cluster.probe_image = references.pop()
    return cluster.probe_image


def probe_pod(
    name,
    namespace,
    image,
    *,
    labels,
    ports,
    node=None,
):
    """Live HTTP listeners, one container per port, that can also run the
    client probe. Containers of a Pod share its network, so every listener has
    the Pod's address."""
    spec = {
        "automountServiceAccountToken": False,
        "terminationGracePeriodSeconds": 0,
        "containers": [
            {
                "name": f"listen-{port}",
                "image": image,
                "imagePullPolicy": "IfNotPresent",
                "env": [
                    {"name": "REGISTRY_HTTP_ADDR", "value": f":{port}"},
                    # The default configuration also binds a debug listener;
                    # containers of one Pod share a network, so give each its
                    # own address or all but the first crash-loop.
                    {
                        "name": "REGISTRY_HTTP_DEBUG_ADDR",
                        "value": f"127.0.0.1:{port + 10000}",
                    },
                ],
                "ports": [{"containerPort": port}],
            }
            for port in ports
        ],
    }
    if node:
        spec["nodeSelector"] = {"kubernetes.io/hostname": node}
    return {
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {"name": name, "namespace": namespace, "labels": labels},
        "spec": spec,
    }


def namespace(name, labels=None):
    return {
        "apiVersion": "v1",
        "kind": "Namespace",
        "metadata": {"name": name, "labels": labels or {}},
    }


def pod_ip(cluster: Cluster, pod, namespace_name):
    document = json.loads(cluster.get("pod", "-n", namespace_name, pod))
    return document["status"].get("podIP", "")


def wait_ready(cluster: Cluster, pods, seconds=180):
    """Waits for every `(namespace, name)` to be Ready with an address.

    A timeout reports each unready pod's phase and container wait reasons:
    bounded, and free of environment values and credentials.
    """

    def state():
        notes = []
        for namespace_name, name in pods:
            document = json.loads(cluster.get("pod", "-n", namespace_name, name))
            status = document["status"]
            conditions = status.get("conditions", [])
            if status.get("podIP") and any(
                c["type"] == "Ready" and c["status"] == "True" for c in conditions
            ):
                continue
            waiting = sorted(
                {
                    (container.get("state", {}).get("waiting") or {}).get("reason", "")
                    for container in status.get("containerStatuses", [])
                }
                - {""}
            )
            notes.append(
                f"{namespace_name}/{name}: {status.get('phase')} {','.join(waiting)}"
            )
        return notes

    deadline = time.monotonic() + seconds
    notes = state()
    while notes and time.monotonic() < deadline:
        time.sleep(1)
        notes = state()
    if notes:
        raise ClusterUnavailable("pods not ready: " + "; ".join(notes[:12]))


_FIRST_CONTAINER: dict = {}


def first_container(cluster: Cluster, pod, namespace_name):
    key = (cluster.name, namespace_name, pod)
    if key not in _FIRST_CONTAINER:
        document = json.loads(cluster.get("pod", "-n", namespace_name, pod))
        _FIRST_CONTAINER[key] = document["spec"]["containers"][0]["name"]
    return _FIRST_CONTAINER[key]


def connects(
    cluster: Cluster, source, namespace_name, address, port, container=None
) -> bool:
    """Whether `source` opens an HTTP connection to `address:port`.

    Exit status alone decides; the probe has its own 3 s deadline.
    """
    container = container or first_container(cluster, source, namespace_name)
    result = cluster.kubectl(
        "exec",
        "-n",
        namespace_name,
        source,
        "-c",
        container,
        "--",
        "wget",
        "-q",
        "-T",
        "3",
        "-t",
        "1",
        "-O",
        "/dev/null",
        f"http://{address}:{port}/v2/",
        check=False,
        timeout=30,
    )
    return result.returncode == 0


def verify_policy_enforcement(cluster: Cluster):
    """Proves the cluster enforces NetworkPolicy before any test relies on it.

    A listener answers a client in the same namespace; a default-deny policy
    must then stop that client, while the listener stays alive for a second
    client the policy admits. A cluster that cannot enforce fails setup here.
    """
    space = f"enforcement-{uuid.uuid4().hex[:8]}"
    image = cluster.probe_image
    labelled = lambda role: {"role": role}  # noqa: E731
    cluster.apply(
        [
            namespace(space),
            probe_pod(
                "listener", space, image, labels=labelled("listener"), ports=[8080]
            ),
            probe_pod(
                "admitted", space, image, labels=labelled("admitted"), ports=[8081]
            ),
            probe_pod(
                "refused", space, image, labels=labelled("refused"), ports=[8082]
            ),
        ]
    )
    pods = [(space, name) for name in ("listener", "admitted", "refused")]
    wait_ready(cluster, pods)
    address = pod_ip(cluster, "listener", space)
    if not connects(cluster, "refused", space, address, 8080):
        raise ClusterUnavailable("the listener is not reachable before any policy")
    cluster.apply(
        [
            {
                "apiVersion": "networking.k8s.io/v1",
                "kind": "NetworkPolicy",
                "metadata": {"name": "listener-only-for-admitted", "namespace": space},
                "spec": {
                    "podSelector": {"matchLabels": {"role": "listener"}},
                    "policyTypes": ["Ingress"],
                    "ingress": [
                        {
                            "from": [
                                {"podSelector": {"matchLabels": {"role": "admitted"}}}
                            ]
                        }
                    ],
                },
            }
        ]
    )

    def enforced():
        return not connects(cluster, "refused", space, address, 8080)

    cluster.wait_for(enforced, 60, "the NetworkPolicy to take effect", interval=2)
    if not connects(cluster, "admitted", space, address, 8080):
        raise ClusterUnavailable("the policy also stopped the admitted client")
    cluster.kubectl("delete", "namespace", space, "--wait=false", check=False)
