"""The chart's own output as inputs for live tests.

Live network tests must not restate the peers, labels and ports by hand: they
take the pod labels from the chart's rendered Deployments, the control-plane
NetworkPolicies from the chart, and the controller's peer selectors and
execution port from the configuration the chart generates. The instance
policies are the renderer's golden objects with exactly those values (and the
namespaces under test) substituted for the renderer fixture's own.
"""

from __future__ import annotations

import copy
import json
import subprocess
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
GOLDEN = (
    ROOT / "services/hermes-manager/crates/hermes-render/tests/golden/instance.json"
)
IMAGE = "ghcr.io/accelero/agent-platform/hermes-manager@sha256:" + "0" * 64
# The renderer fixture's values, which the substitution replaces.
FIXTURE_CONTROL_NAMESPACE = "hermes-system"
FIXTURE_PEERS = {
    "gateway": {"app.kubernetes.io/component": "hermes-gateway"},
    "controller": {"app.kubernetes.io/component": "hermes-controller"},
    "api": {"app.kubernetes.io/component": "hermes-manager-api"},
}
FIXTURE_EXECUTION_PORT = 8443


def values():
    document = yaml.safe_load((ROOT / "examples/manager/values.yaml").read_text())
    document["images"] = {"hermes-manager": IMAGE}
    return document


def template(document, namespace):
    result = subprocess.run(
        [
            "helm",
            "template",
            "fixture",
            str(ROOT / "charts/agent-platform"),
            "-n",
            namespace,
            "-f",
            "-",
        ],
        input=yaml.safe_dump(document),
        capture_output=True,
        text=True,
        timeout=60,
    )
    if result.returncode != 0:
        raise RuntimeError("helm template failed")
    return [item for item in yaml.safe_load_all(result.stdout) if item]


class Chart:
    """What the chart installs and the controller is configured with."""

    def __init__(self):
        document = values()
        self.installation = document["hermesManager"]["installation"]["id"]
        probe = template(document, "agents")
        controller = next(
            item
            for item in probe
            if item["kind"] == "ConfigMap"
            and item["metadata"]["name"] == "hermes-manager-controller"
        )
        self.config = json.loads(controller["data"]["config.json"])["controller"]
        self.namespace = self.config["namespace"]
        self.documents = template(document, self.namespace)
        self.peers = self.config["peers"]
        self.execution_port = self.config["executionPort"]

    def policies(self):
        """The control-plane NetworkPolicies the chart installs."""
        return [item for item in self.documents if item["kind"] == "NetworkPolicy"]

    def pod_labels(self, name):
        deployment = next(
            item
            for item in self.documents
            if item["kind"] == "Deployment" and item["metadata"]["name"] == name
        )
        return dict(deployment["spec"]["template"]["metadata"]["labels"])

    def container_ports(self, name):
        deployment = next(
            item
            for item in self.documents
            if item["kind"] == "Deployment" and item["metadata"]["name"] == name
        )
        return sorted(
            port["containerPort"]
            for container in deployment["spec"]["template"]["spec"]["containers"]
            for port in container.get("ports", [])
        )

    def instance_policies(self, namespace):
        """The renderer's instance NetworkPolicies, configured as the chart
        configures the controller, placed in `namespace`."""
        rendered = [
            copy.deepcopy(item)
            for item in json.loads(GOLDEN.read_text())
            if item["kind"] == "NetworkPolicy"
        ]
        replacements = {
            json.dumps(FIXTURE_PEERS[role], sort_keys=True): self.peers[role]
            for role in FIXTURE_PEERS
        }
        control = {"kubernetes.io/metadata.name": FIXTURE_CONTROL_NAMESPACE}

        def walk(node):
            if isinstance(node, dict):
                key = json.dumps(node, sort_keys=True)
                if key in replacements:
                    return copy.deepcopy(replacements[key])
                if node == control:
                    return {"kubernetes.io/metadata.name": self.namespace}
                return {name: walk(value) for name, value in node.items()}
            if isinstance(node, list):
                return [walk(item) for item in node]
            return node

        result = []
        for item in rendered:
            item = walk(item)
            item["metadata"]["namespace"] = namespace
            for rule in item["spec"].get("egress", []):
                selects_api = any(
                    peer.get("podSelector", {}).get("matchLabels") == self.peers["api"]
                    for peer in rule.get("to", [])
                )
                if selects_api:
                    for port in rule["ports"]:
                        assert port["port"] == FIXTURE_EXECUTION_PORT
                        port["port"] = self.execution_port
            text = json.dumps(item)
            assert FIXTURE_CONTROL_NAMESPACE not in text
            assert 'app.kubernetes.io/component": "hermes-' not in text
            result.append(item)
        return result
