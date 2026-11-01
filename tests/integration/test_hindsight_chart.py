"""Hindsight's optional chart contract, using real Helm renders (H1–H13)."""

import hashlib
import json
from copy import deepcopy

import pytest
import yaml

from agent_platform.build import ROOT
from tests.test_chart import render

pytestmark = pytest.mark.integration
HINDSIGHT_IMAGE = (
    "ghcr.io/vectorize-io/hindsight-api:0.10.3@sha256:"
    "f80f304db313cbc8775acd601fda7d094f5854ebf9bc252d8134eb9276d56b40"
)


@pytest.fixture
def hindsight_values():
    values = yaml.safe_load((ROOT / "examples/lightweight/values.yaml").read_text())
    values.update(yaml.safe_load((ROOT / "examples/hindsight/values.yaml").read_text()))
    values["workloads"].update(
        yaml.safe_load((ROOT / "examples/lightweight/values.yaml").read_text())[
            "workloads"
        ]
    )
    return values


def pod(objects, name="hindsight"):
    return objects["Deployment", name]["spec"]["template"]["spec"]


def container(objects, name="hindsight"):
    return pod(objects, name)["containers"][0]


def environment(objects, name="hindsight"):
    return {item["name"]: item for item in container(objects, name)["env"]}


def defaults(objects):
    return json.loads(objects["ConfigMap", "hermes-defaults"]["data"]["defaults.json"])


def test_h1_disabled_chart_renders_no_hindsight_and_unchanged_defaults(tmp_path):
    values = yaml.safe_load((ROOT / "examples/lightweight/values.yaml").read_text())
    objects = render(tmp_path, values)
    assert not any("hindsight" in name for _, name in objects)
    assert not any(
        name.startswith("HINDSIGHT_") for name in environment(objects, "hermes")
    )
    baseline = json.loads(
        (ROOT / "tests/support/hindsight-disabled-defaults.json").read_text()
    )
    assert (
        objects["ConfigMap", "hermes-defaults"]["data"]["defaults.json"]
        == baseline["defaults"]
    )
    assert (
        objects["Deployment", "hermes"]["spec"]["template"]["metadata"]["annotations"][
            "checksum/hermes-defaults"
        ]
        == baseline["checksum"]
    )


def test_h2_enabled_chart_renders_pinned_api_without_placeholders(
    tmp_path, hindsight_values
):
    objects = render(tmp_path, hindsight_values)
    api = container(objects)
    assert api["image"] == HINDSIGHT_IMAGE
    assert environment(objects)["HINDSIGHT_API_DATABASE_URL"]["valueFrom"][
        "secretKeyRef"
    ] == {"name": "hindsight-database", "key": "url"}
    assert "$(" not in json.dumps(api)
    assert api["command"][:2] == ["/bin/sh", "-c"]
    assert "postgres://*|postgresql://*) exec /app/start-all.sh" in api["command"][2]
    assert "args" not in api
    assert objects["Deployment", "hindsight"]["spec"]["strategy"] == {
        "type": "Recreate"
    }
    env = environment(objects)
    assert env["HINDSIGHT_API_WORKER_ENABLED"]["value"] == "true"
    assert env["HINDSIGHT_API_WORKER_ID"]["value"] == "hindsight"
    assert objects["Service", "hindsight"]["spec"]["ports"][0]["port"] == 8888


@pytest.mark.parametrize(
    "section,key",
    [
        ("database", "urlSecretRef"),
        ("llm", "provider"),
        ("llm", "model"),
        ("workload", "image"),
    ],
)
def test_h2_missing_inputs_fail_the_render(tmp_path, hindsight_values, section, key):
    if section == "workload":
        hindsight_values["workloads"]["hindsight"][key] = "example.org/unpinned:latest"
    else:
        hindsight_values["hindsight"][section][key] = (
            {"name": "", "key": "url"} if key == "urlSecretRef" else ""
        )
    error = render(tmp_path, hindsight_values, valid=False)
    assert "hindsight" in error


@pytest.mark.parametrize(
    "workload,names",
    [
        ("hermes", ["HINDSIGHT_MODE", "HINDSIGHT_BANK_ID", "HINDSIGHT_API_URL"]),
        (
            "hindsight",
            [
                "HINDSIGHT_API_DATABASE_URL",
                "HINDSIGHT_API_LLM_PROVIDER",
                "HINDSIGHT_API_LLM_MODEL",
                "HINDSIGHT_API_LLM_BASE_URL",
                "HINDSIGHT_API_LLM_API_KEY",
                "HINDSIGHT_API_WORKER_ENABLED",
                "HINDSIGHT_API_WORKER_ID",
            ],
        ),
    ],
)
@pytest.mark.parametrize("field", ["env", "secretEnv"])
def test_h2_chart_owned_environment_cannot_be_shadowed(
    tmp_path, hindsight_values, workload, names, field
):
    for name in names:
        hindsight_values["workloads"].setdefault(workload, {})[field] = [
            {"name": name, "value": "override", "key": "override"}
        ]
        assert "chart-owned" in render(tmp_path, hindsight_values, valid=False)


def test_h3_explicit_root_provider_is_not_seeded(tmp_path, hindsight_values):
    for provider in ("", "other", "hindsight"):
        hindsight_values["hermes"]["config"] = {"memory": {"provider": provider}}
        seed = defaults(render(tmp_path, hindsight_values))
        assert seed["config"]["memory"]["provider"] == provider
        assert "hindsight" not in seed


def test_h4_chart_profile_does_not_inherit_the_root_provider(
    tmp_path, hindsight_values
):
    hindsight_values["hermes"]["profiles"] = {"research": {"model": "chosen"}}
    objects = render(tmp_path, hindsight_values)
    seed = defaults(objects)
    assert seed["config"]["memory"]["provider"] == "hindsight"
    assert seed["profiles"]["research"]["memory"]["provider"] == ""
    assert "hindsight" not in seed["profiles"]["research"]
    raw = objects["ConfigMap", "hermes-defaults"]["data"]["defaults.json"]
    checksum = objects["Deployment", "hermes"]["spec"]["template"]["metadata"][
        "annotations"
    ]["checksum/hermes-defaults"]
    assert checksum == hashlib.sha256(raw.encode()).hexdigest()


def test_h4_explicit_profile_provider_is_preserved(tmp_path, hindsight_values):
    hindsight_values["hermes"]["profiles"] = {
        "research": {"memory": {"provider": "hindsight"}},
        "coding": {"memory": {"provider": ""}},
        "other": {"memory": {"provider": "other"}},
    }
    seed = defaults(render(tmp_path, hindsight_values))
    assert seed["profiles"] == hindsight_values["hermes"]["profiles"]


def test_h8_credentials_render_only_as_secret_references(tmp_path, hindsight_values):
    hindsight_values["hindsight"]["llm"]["baseUrl"] = "https://llm.example.org/v1"
    hindsight_values["workloads"]["hindsight"]["env"] = [
        {"name": "CUSTOM", "value": "preserved"}
    ]
    objects = render(tmp_path, hindsight_values)
    env = environment(objects)
    assert env["HINDSIGHT_API_LLM_API_KEY"]["valueFrom"]["secretKeyRef"] == {
        "name": "hindsight-llm",
        "key": "api-key",
    }
    assert env["HINDSIGHT_API_LLM_BASE_URL"]["value"] == "https://llm.example.org/v1"
    assert env["CUSTOM"]["value"] == "preserved"
    assert env["HINDSIGHT_API_LLM_PROVIDER"]["value"] == "openai"
    assert not any(kind == "Secret" for kind, _ in objects)
    assert defaults(objects)["hindsight"] == {
        "mode": "local_external",
        "api_url": "http://hindsight:8888",
        "bank_id": "default",
    }
    for (kind, _), obj in objects.items():
        if kind == "ConfigMap":
            assert "hindsight-database" not in json.dumps(obj)
            assert "hindsight-llm" not in json.dumps(obj)
    hindsight_values["hindsight"]["llm"]["apiKeySecretRef"]["name"] = ""
    assert "HINDSIGHT_API_LLM_API_KEY" not in environment(
        render(tmp_path, hindsight_values)
    )


@pytest.mark.parametrize(
    "url",
    [
        "https://user:password@llm.example.org/v1",
        "https://llm.example.org?api_key=synthetic",
    ],
)
def test_h8_llm_endpoint_cannot_embed_a_credential(tmp_path, hindsight_values, url):
    hindsight_values["hindsight"]["llm"]["baseUrl"] = url
    error = render(tmp_path, hindsight_values, valid=False)
    assert "without credentials" in error
    assert url not in error


def test_h9_pod_is_non_root_without_token_or_rbac(tmp_path, hindsight_values):
    objects = render(tmp_path, hindsight_values)
    spec = pod(objects)
    assert spec["automountServiceAccountToken"] is False
    assert "serviceAccountName" not in spec
    assert spec["securityContext"] == {
        "runAsNonRoot": True,
        "runAsUser": 1000,
        "runAsGroup": 1000,
        "fsGroup": 1000,
        "seccompProfile": {"type": "RuntimeDefault"},
    }
    assert container(objects)["securityContext"] == {
        "allowPrivilegeEscalation": False,
        "capabilities": {"drop": ["ALL"]},
    }
    for (kind, _), obj in objects.items():
        if kind in {
            "Role",
            "RoleBinding",
            "ClusterRole",
            "ClusterRoleBinding",
            "ServiceAccount",
        }:
            assert "hindsight" not in json.dumps(obj)


def test_h9_only_hermes_gets_ingress(tmp_path, hindsight_values):
    objects = render(tmp_path, hindsight_values)
    policy = objects["NetworkPolicy", "hindsight-ingress"]["spec"]
    assert policy["policyTypes"] == ["Ingress"]
    assert policy["ingress"] == [
        {
            "from": [
                {
                    "podSelector": {
                        "matchLabels": {
                            "app.kubernetes.io/instance": "agents",
                            "app.kubernetes.io/name": "hermes",
                        }
                    }
                }
            ],
            "ports": [{"protocol": "TCP", "port": 8888}],
        }
    ]
    assert not any(
        "hindsight" in name and name != "hindsight-ingress"
        for kind, name in objects
        if kind == "NetworkPolicy"
    )


def test_h9_enabled_api_requires_network_policy(tmp_path, hindsight_values):
    hindsight_values["networkPolicy"] = {"enabled": False}
    error = render(tmp_path, hindsight_values, valid=False)
    assert "workloads.hindsight.enabled requires networkPolicy.enabled: true" in error
    assert "no authentication" in error
    hindsight_values["workloads"]["hindsight"]["enabled"] = False
    assert ("Deployment", "hindsight") not in render(tmp_path, hindsight_values)


def test_h9_only_hindsight_gets_its_credentials(tmp_path, hindsight_values):
    objects = render(tmp_path, hindsight_values)
    for (kind, name), obj in objects.items():
        if kind == "Deployment" and name != "hindsight":
            assert not {
                "hindsight-database",
                "hindsight-llm",
                "hindsight-postgres-ca",
            }.intersection(_strings(obj))


def _strings(value):
    if isinstance(value, dict):
        return set().union(*(_strings(item) for item in value.values()), set())
    if isinstance(value, list):
        return set().union(*(_strings(item) for item in value), set())
    return {value} if isinstance(value, str) else set()


def test_h10_probes_are_unauthenticated_http(tmp_path, hindsight_values):
    api = container(render(tmp_path, hindsight_values))
    assert api["readinessProbe"]["httpGet"] == {"path": "/health", "port": "http"}
    assert api["startupProbe"] == {
        "httpGet": {"path": "/health/live", "port": "http"},
        "periodSeconds": 5,
        "failureThreshold": 60,
    }
    assert "livenessProbe" not in api


def test_h11_default_has_no_persistent_volume(tmp_path, hindsight_values):
    objects = render(tmp_path, hindsight_values)
    assert not any(
        "persistentVolumeClaim" in volume or "emptyDir" in volume
        for volume in pod(objects).get("volumes", [])
    )
    assert not any(
        "hindsight" in name for kind, name in objects if kind == "PersistentVolumeClaim"
    )
    ca = pod(objects)["volumes"][0]
    assert ca == {
        "name": "postgres-ca",
        "secret": {"secretName": "hindsight-postgres-ca"},
    }
    hindsight_values["hindsight"]["database"]["caSecret"] = ""
    assert not pod(render(tmp_path, hindsight_values)).get("volumes")


@pytest.mark.parametrize(
    "enabled,existing", [(False, ""), (True, ""), (False, "retained-cache")]
)
def test_h11_offline_models_only_without_cache_claim(
    tmp_path, hindsight_values, enabled, existing
):
    hindsight_values["persistence"]["hindsight-cache"] = {
        "enabled": enabled,
        "existingClaim": existing,
    }
    entries = [
        entry
        for entry in container(render(tmp_path, hindsight_values))["env"]
        if entry["name"] == "HF_HUB_OFFLINE"
    ]
    assert entries == (
        [] if enabled or existing else [{"name": "HF_HUB_OFFLINE", "value": "1"}]
    )


@pytest.mark.parametrize(
    "enabled,existing", [(False, ""), (True, ""), (False, "retained-cache")]
)
@pytest.mark.parametrize("value", ["0", ""])
@pytest.mark.parametrize("field", ["env", "secretEnv"])
def test_h11_operator_offline_setting_is_preserved(
    tmp_path, hindsight_values, enabled, existing, value, field
):
    hindsight_values["persistence"]["hindsight-cache"] = {
        "enabled": enabled,
        "existingClaim": existing,
    }
    entry = {"name": "HF_HUB_OFFLINE", "value": value}
    expected = entry
    if field == "secretEnv":
        entry = {
            "name": "HF_HUB_OFFLINE",
            "secretName": "offline-setting",
            "key": "value",
        }
        expected = {
            "name": "HF_HUB_OFFLINE",
            "valueFrom": {"secretKeyRef": {"name": "offline-setting", "key": "value"}},
        }
    hindsight_values["workloads"]["hindsight"][field] = [entry]
    entries = [
        entry
        for entry in container(render(tmp_path, hindsight_values))["env"]
        if entry["name"] == "HF_HUB_OFFLINE"
    ]
    assert entries == [expected]


@pytest.mark.parametrize("enabled,existing", [(True, ""), (False, "retained-cache")])
def test_h11_cache_claim_mounts_only_when_selected(
    tmp_path, hindsight_values, enabled, existing
):
    hindsight_values["persistence"]["hindsight-cache"] = {
        "enabled": enabled,
        "existingClaim": existing,
    }
    objects = render(tmp_path, hindsight_values)
    assert {
        "name": "hindsight-cache",
        "persistentVolumeClaim": {"claimName": existing or "hindsight-cache"},
    } in pod(objects)["volumes"]
    assert {
        "name": "hindsight-cache",
        "mountPath": "/home/hindsight/.cache",
        "readOnly": False,
    } in container(objects)["volumeMounts"]
    if enabled:
        claim = objects["PersistentVolumeClaim", "hindsight-cache"]
        assert claim["metadata"]["annotations"]["helm.sh/resource-policy"] == "keep"
        disabled = deepcopy(hindsight_values)
        disabled["workloads"]["hindsight"]["enabled"] = False
        assert (
            render(tmp_path, disabled)["PersistentVolumeClaim", "hindsight-cache"]
            == claim
        )


def test_h11_retired_login_claim_is_not_rendered_or_deleted(tmp_path, hindsight_values):
    source = yaml.safe_load((ROOT / "charts/agent-platform/values.yaml").read_text())
    assert "hindsight-login" not in source["persistence"]
    objects = render(tmp_path, hindsight_values)
    assert all("hindsight-login" not in json.dumps(obj) for obj in objects.values())
    assert "hindsight" not in source["bootstrap"]["writers"]
    assert "keep" in (ROOT / "charts/agent-platform/templates/storage.yaml").read_text()


def test_h13_hermes_gets_internal_endpoint_fail_safe(tmp_path, hindsight_values):
    env = environment(render(tmp_path, hindsight_values), "hermes")
    assert {
        name: env[name]["value"]
        for name in ("HINDSIGHT_MODE", "HINDSIGHT_BANK_ID", "HINDSIGHT_API_URL")
    } == {
        "HINDSIGHT_MODE": "local_external",
        "HINDSIGHT_BANK_ID": "default",
        "HINDSIGHT_API_URL": "http://hindsight:8888",
    }
    assert "HINDSIGHT_API_KEY" not in env
