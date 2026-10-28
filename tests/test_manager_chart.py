"""Render the personal Hermes chart from synthetic values only.

The admission policies and controller RBAC must equal hermes-render's golden
output, and each role's generated configuration must equal the fixture the
Rust configuration parsers load. Set UPDATE_GOLDEN=1 to rewrite the fixtures.
"""

import copy
import json
import os
import subprocess
from pathlib import Path

import pytest
import yaml

pytestmark = pytest.mark.integration

ROOT = Path(__file__).resolve().parents[1]
CRATES = ROOT / "services/hermes-manager/crates"
RENDER_GOLDEN = CRATES / "hermes-render/tests/golden"
CONFIG_FIXTURES = {
    "hermes-manager-api": CRATES / "hermes-manager/tests/chart/api.json",
    "hermes-manager-controller": CRATES / "hermes-manager/tests/chart/controller.json",
    "hermes-gateway": CRATES / "hermes-gateway/tests/chart/gateway.json",
}
IMAGE = "ghcr.io/accelero/agent-platform/hermes-manager@sha256:" + "0" * 64


def example():
    values = yaml.safe_load((ROOT / "examples/manager/values.yaml").read_text())
    values["images"] = {"hermes-manager": IMAGE}
    return values


def renderer_fixture():
    """The installation hermes-render's golden files were rendered for."""
    values = example()
    manager = values["hermesManager"]
    manager["installation"]["id"] = "7d6c0e1a-0000-4000-8000-000000000001"
    manager["controller"]["instanceRole"] = "hermes-manager-instance"
    return values


def render(tmp_path, values, namespace="hermes-system", check=True):
    path = tmp_path / "values.yaml"
    path.write_text(yaml.safe_dump(values))
    result = subprocess.run(
        [
            "helm",
            "template",
            "fixture",
            str(ROOT / "charts/agent-platform"),
            "-n",
            namespace,
            "-f",
            str(path),
        ],
        capture_output=True,
        text=True,
        timeout=30,
    )
    if not check:
        return result
    assert result.returncode == 0, result.stderr
    return [value for value in yaml.safe_load_all(result.stdout) if value]


def canonical(documents):
    return sorted(json.dumps(document, sort_keys=True) for document in documents)


def walk(value, keys=None, files=None):
    """Every key, and every value of a `...File` or `...Directory` key."""
    keys = set() if keys is None else keys
    files = [] if files is None else files
    if isinstance(value, dict):
        for key, item in value.items():
            keys.add(key)
            if key.endswith(("File", "Directory")):
                files.append(item)
            walk(item, keys, files)
    elif isinstance(value, list):
        for item in value:
            walk(item, keys, files)
    return keys, files


def named(documents, kind, name):
    return next(
        x for x in documents if x["kind"] == kind and x["metadata"]["name"] == name
    )


def test_admission_policies_and_rbac_equal_the_renderer(tmp_path):
    documents = render(tmp_path, renderer_fixture())
    rbac = [
        x
        for x in documents
        if x["kind"] in {"ClusterRole", "ClusterRoleBinding", "Role", "RoleBinding"}
    ]
    policies = [
        x for x in documents if x["kind"].startswith("ValidatingAdmissionPolicy")
    ]
    for objects, golden in [(rbac, "rbac.json"), (policies, "policies.json")]:
        expected = json.loads((RENDER_GOLDEN / golden).read_text())
        assert canonical(objects) == canonical(expected), golden


def test_role_configurations_equal_the_parsed_fixtures(tmp_path):
    documents = render(tmp_path, example(), namespace="agents")
    for name, fixture in CONFIG_FIXTURES.items():
        config = json.loads(named(documents, "ConfigMap", name)["data"]["config.json"])
        if os.environ.get("UPDATE_GOLDEN"):
            fixture.parent.mkdir(parents=True, exist_ok=True)
            fixture.write_text(json.dumps(config, indent=2, sort_keys=True) + "\n")
        assert config == json.loads(fixture.read_text()), name
        keys, files = walk(config)
        assert not keys & {"token", "password", "secret", "clientSecret", "key"}, name
        assert files and all(
            path.startswith(
                (
                    "/run/hermes/secrets/",
                    "/run/hermes/config/",
                    "/usr/share/hermes-manager/",
                )
            )
            for path in files
        ), name
    keyrings = {
        name: json.loads(named(documents, "ConfigMap", name)["data"]["keyring.json"])
        for name in ("hermes-manager-api", "hermes-gateway")
    }
    assert keyrings["hermes-gateway"] == {
        "activeKeyId": "gateway-2026-10",
        "keyFiles": {
            "gateway-2026-10": "/run/hermes/secrets/session-key-gateway-2026-10"
        },
    }


def test_only_the_controller_carries_a_token_and_every_pod_is_restricted(tmp_path):
    documents = render(tmp_path, example())
    deployments = {
        x["metadata"]["name"]: x["spec"]["template"]["spec"]
        for x in documents
        if x["kind"] == "Deployment"
    }
    assert set(deployments) == {
        "hermes-manager-api",
        "hermes-manager-controller",
        "hermes-gateway",
    }
    for name, pod in deployments.items():
        assert pod["automountServiceAccountToken"] is (
            name == "hermes-manager-controller"
        ), name
        assert pod["securityContext"]["runAsNonRoot"] is True
        assert pod["securityContext"]["runAsUser"] == 10001
        container = pod["containers"][0]
        assert container["image"] == IMAGE
        assert container["securityContext"]["readOnlyRootFilesystem"] is True
        assert container["securityContext"]["allowPrivilegeEscalation"] is False
        assert container["securityContext"]["capabilities"]["drop"] == ["ALL"]
        secrets = next(x for x in pod["volumes"] if x["name"] == "secrets")
        assert secrets["secret"]["defaultMode"] == 0o440
    controller = deployments["hermes-manager-controller"]
    assert controller["serviceAccountName"] == "hermes-manager-controller"
    assert deployments["hermes-gateway"]["containers"][0]["command"] == [
        "hermes-gateway"
    ]
    assert deployments["hermes-manager-api"]["containers"][0]["args"] == [
        "api",
        "--config",
        "/run/hermes/config/config.json",
    ]
    # No Secret is ever rendered; every role reads its own existing one.
    assert not any(x["kind"] == "Secret" for x in documents)
    volumes = {
        name: next(x for x in pod["volumes"] if x["name"] == "secrets")["secret"][
            "secretName"
        ]
        for name, pod in deployments.items()
    }
    assert len(set(volumes.values())) == 3


def test_network_policies_admit_only_named_peers(tmp_path):
    documents = render(tmp_path, example())
    api = named(documents, "NetworkPolicy", "hermes-manager-api")["spec"]["ingress"]
    ports = {rule["ports"][0]["port"]: rule["from"] for rule in api}
    assert set(ports) == {8443, 8444, 8445}
    assert ports[8444] == [
        {
            "podSelector": {
                "matchLabels": {
                    "app.kubernetes.io/instance": "fixture",
                    "app.kubernetes.io/name": "hermes-gateway",
                }
            }
        }
    ]
    assert ports[8445][0]["namespaceSelector"] == {
        "matchLabels": {
            "agent-platform.io/installation": "00000000-0000-4000-8000-000000000001"
        }
    }
    controller = named(documents, "NetworkPolicy", "hermes-manager-controller")
    assert controller["spec"]["ingress"] == []
    gateway = named(documents, "NetworkPolicy", "hermes-gateway")["spec"]["ingress"]
    assert [rule["ports"][0]["port"] for rule in gateway] == [8443]
    without_peers = example()
    without_peers["hermesManager"]["ingress"] = {"admin": [], "gateway": []}
    documents = render(tmp_path, without_peers)
    gateway = named(documents, "NetworkPolicy", "hermes-gateway")
    assert gateway["spec"]["ingress"] == []
    api = named(documents, "NetworkPolicy", "hermes-manager-api")["spec"]["ingress"]
    assert 8443 not in {rule["ports"][0]["port"] for rule in api}


@pytest.mark.parametrize(
    ("change", "message"),
    [
        (lambda m: m["installation"].update(id="NOT-A-UUID"), "lowercase UUID"),
        (lambda m: m["installation"].update(issuer="http://sso"), "HTTPS"),
        (lambda m: m["api"].update(existingSecret=""), "existingSecret"),
        (lambda m: m["gateway"]["sessionKeys"].update(active="other"), "active key"),
        (lambda m: m["api"].update(gatewayIdentity=""), "gatewayIdentity"),
        (lambda m: m.update(publicOrigin=""), "publicOrigin"),
        (
            lambda m: m["storageSources"].append(
                {"id": "nvme", "storageClass": {"name": "other"}}
            ),
            "duplicate",
        ),
        (lambda m: m.update(unexpected=True), "unexpected"),
        (lambda m: m["api"].update(config={}), "config"),
    ],
)
def test_incomplete_or_unknown_values_fail_the_render(tmp_path, change, message):
    values = copy.deepcopy(example())
    change(values["hermesManager"])
    result = render(tmp_path, values, check=False)
    assert result.returncode != 0
    assert message in result.stderr


def test_operator_jobs_run_once_per_revision_with_their_own_secrets(tmp_path):
    values = example()
    values["hermesManager"]["operations"] = {
        "migrate": {"enabled": True, "existingSecret": "hermes-manager-schema-owner"},
        "initialize": {"enabled": True},
        "rebindCluster": {"enabled": True},
        "grantAdministrator": {
            "enabled": True,
            "existingSecret": "hermes-first-administrator",
            "recover": False,
        },
    }
    documents = render(tmp_path, values)
    jobs = {x["metadata"]["name"]: x for x in documents if x["kind"] == "Job"}
    assert set(jobs) == {
        "hermes-manager-migrate-1",
        "hermes-manager-initialize-1",
        "hermes-manager-rebind-cluster-1",
        "hermes-manager-grant-administrator-1",
    }
    pods = {name: job["spec"]["template"]["spec"] for name, job in jobs.items()}
    for name, pod in pods.items():
        # The mounted Secret's files are group-readable only for this group.
        assert pod["securityContext"]["fsGroup"] == 10001, name
        assert pod["volumes"][1]["secret"]["defaultMode"] == 0o440, name
    for name in ("hermes-manager-initialize-1", "hermes-manager-rebind-cluster-1"):
        assert pods[name]["automountServiceAccountToken"] is True
        assert pods[name]["serviceAccountName"] == "hermes-manager-controller"
    assert pods["hermes-manager-migrate-1"]["automountServiceAccountToken"] is False
    assert pods["hermes-manager-grant-administrator-1"]["containers"][0]["args"] == [
        "grant-administrator",
        "--identity-file",
        "/run/hermes/secrets/identity.json",
        "--config",
        "/run/hermes/config/config.json",
    ]
    migrate = json.loads(
        named(documents, "ConfigMap", "hermes-manager-migrate")["data"]["config.json"]
    )
    assert set(migrate) == {
        "installation",
        "databaseUrlFile",
        "storageSources",
        "dataIdentity",
    }
