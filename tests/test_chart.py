"""Exercise native application configuration and isolation through real Helm renders."""

import json
import re
import subprocess
from copy import deepcopy
from pathlib import Path

import pytest
import yaml

from agent_platform.build import ROOT, TARGETS

CHART = ROOT / "charts/agent-platform"
IMAGE = "ghcr.io/fixture/agent-runtime@sha256:" + "b" * 64


@pytest.fixture
def values():
    value = yaml.safe_load((ROOT / "examples/lightweight/values.yaml").read_text())
    value["images"] = {
        name: f"ghcr.io/fixture/{name}@sha256:" + "a" * 64 for name in TARGETS
    }
    return value


def render(tmp_path, values, *, valid=True):
    path = tmp_path / "values.yaml"
    path.write_text(yaml.safe_dump(values))
    result = subprocess.run(
        [
            "helm",
            "template",
            "agents",
            str(CHART),
            "--namespace",
            "agents",
            "--values",
            str(path),
        ],
        capture_output=True,
        text=True,
        timeout=30,
    )
    if not valid:
        assert result.returncode != 0
        return result.stderr
    assert result.returncode == 0, result.stderr
    documents = [item for item in yaml.safe_load_all(result.stdout) if item]
    return {(item["kind"], item["metadata"]["name"]): item for item in documents}


def native(objects):
    return yaml.safe_load(
        objects["ConfigMap", "runtime-manager-config"]["data"]["config.yaml"]
    )


def policy(objects):
    item = next(
        value
        for (kind, name), value in objects.items()
        if kind == "ValidatingAdmissionPolicy" and not name.endswith("-delete")
    )
    return "\n".join(rule["expression"] for rule in item["spec"]["validations"])


def test_complete_application_uses_chart_defaults_and_named_credentials(
    tmp_path, values
):
    objects = render(tmp_path, values)
    deployed = {name for kind, name in objects if kind == "Deployment"}
    assert deployed == {
        "hermes",
        "runtime-manager",
        "multica-backend",
        "multica-web",
        "multica-gateway",
        "hermes-gateway",
    }
    config = native(objects)
    assert config["backend"] == "kubernetes"
    assert config["kubernetes"] == {"namespace": "agent-execution"}
    template = config["templates"]["personal-assistant"]
    assert template["image"] == values["images"]["agent-runtime"]
    assert template["kubernetes"]["uid"] == 1000
    assert values["images"]["agent-runtime"] in policy(objects)
    manager = objects["Deployment", "runtime-manager"]["spec"]["template"]["spec"]
    auth = next(
        volume for volume in manager["volumes"] if volume["name"] == "manager-token"
    )["secret"]
    assert auth["items"] == [{"key": "hermes-token", "path": "hermes"}]
    assert (
        config["clients"]["hermes"]["token_file"] == "/run/runtime-manager-auth/hermes"
    )
    assert not any(kind == "Secret" for kind, _name in objects)
    assert objects["Service", "multica-gateway"]["spec"]["ports"][0]["port"] == 80
    assert (
        "location /api/"
        in objects["ConfigMap", "multica-gateway-config"]["data"]["nginx.conf"]
    )
    env = objects["Deployment", "hermes"]["spec"]["template"]["spec"]["containers"][0][
        "env"
    ]
    assert not {entry["name"] for entry in env} & {
        "FIRECRAWL_API_URL",
        "SEARXNG_URL",
        "CAMOFOX_URL",
    }


@pytest.fixture
def oidc_values(values: dict) -> dict:
    """Select a public PKCE client using only supported chart inputs."""
    values["hermes"]["dashboardAuth"] = {
        "mode": "oidc",
        "oidc": {
            "issuer": "https://sso.example.org/realms/example",
            "clientId": "hermes",
            "publicUrl": "https://hermes.example.org",
        },
    }
    return values


def test_oidc_replaces_basic_auth_without_replacing_workload_lists(
    tmp_path: Path, oidc_values: dict
) -> None:
    """OIDC removes both literal and Secret-based Basic auth while keeping API auth."""
    oidc_values["workloads"]["hermes"]["env"] = [
        {"name": "HERMES_DASHBOARD_BASIC_AUTH_PASSWORD", "value": "legacy-fixture"},
        {"name": "KEEP_CUSTOM_SETTING", "value": "preserved"},
    ]
    objects = render(tmp_path, oidc_values)
    container = objects["Deployment", "hermes"]["spec"]["template"]["spec"][
        "containers"
    ][0]
    env = {item["name"]: item for item in container["env"]}
    assert not any(name.startswith("HERMES_DASHBOARD_BASIC_AUTH_") for name in env)
    assert env["HERMES_DASHBOARD_OIDC_ISSUER"]["value"] == (
        "https://sso.example.org/realms/example"
    )
    assert env["HERMES_DASHBOARD_OIDC_CLIENT_ID"]["value"] == "hermes"
    assert env["HERMES_DASHBOARD_OIDC_SCOPES"]["value"] == "openid email profile"
    assert env["HERMES_DASHBOARD_PUBLIC_URL"]["value"] == "https://hermes.example.org"
    assert "HERMES_DASHBOARD_OIDC_CLIENT_SECRET" not in env
    assert env["KEEP_CUSTOM_SETTING"]["value"] == "preserved"
    assert env["API_SERVER_KEY"]["valueFrom"]["secretKeyRef"]["key"] == "API_SERVER_KEY"
    assert env["EXAMPLE_API_KEY"]["valueFrom"]["secretKeyRef"]["name"] == (
        "hermes-provider-credentials"
    )
    assert len(env) == len(container["env"])
    assert not any(kind == "Secret" for kind, _name in objects)


def test_oidc_confidential_client_uses_only_a_named_secret_key(
    tmp_path: Path, oidc_values: dict
) -> None:
    """A confidential client references one existing key without embedding a value."""
    oidc_values["hermes"]["dashboardAuth"]["oidc"]["clientSecretRef"] = {
        "name": "dashboard-oidc",
        "key": "client",
    }
    objects = render(tmp_path, oidc_values)
    env = objects["Deployment", "hermes"]["spec"]["template"]["spec"]["containers"][0][
        "env"
    ]
    secret = next(
        item for item in env if item["name"] == "HERMES_DASHBOARD_OIDC_CLIENT_SECRET"
    )
    assert secret == {
        "name": "HERMES_DASHBOARD_OIDC_CLIENT_SECRET",
        "valueFrom": {"secretKeyRef": {"name": "dashboard-oidc", "key": "client"}},
    }


def test_default_basic_auth_is_preserved(tmp_path: Path, values: dict) -> None:
    """Existing chart consumers retain their username and password Secret inputs."""
    values["hermes"]["dashboardUsername"] = "operator"
    objects = render(tmp_path, values)
    env = {
        item["name"]: item
        for item in objects["Deployment", "hermes"]["spec"]["template"]["spec"][
            "containers"
        ][0]["env"]
    }
    assert env["HERMES_DASHBOARD_BASIC_AUTH_USERNAME"]["value"] == "operator"
    for name, key in (
        ("PASSWORD", "DASHBOARD_PASSWORD"),
        ("SECRET", "DASHBOARD_SECRET"),
    ):
        assert env["HERMES_DASHBOARD_BASIC_AUTH_" + name]["valueFrom"][
            "secretKeyRef"
        ] == {"name": "agent-platform-secrets", "key": key}
    assert not any(name.startswith("HERMES_DASHBOARD_OIDC_") for name in env)


@pytest.mark.parametrize(
    ("key", "value"),
    [
        ("issuer", ""),
        ("issuer", "http://sso.example.org"),
        ("issuer", "https://user@sso.example.org"),
        ("issuer", "https://sso.example.org?query=value"),
        ("issuer", "https://sso.example.org#fragment"),
        ("clientId", ""),
        ("clientId", "has spaces"),
        ("publicUrl", ""),
        ("publicUrl", "http://hermes.example.org"),
        ("publicUrl", "https://hermes.example.org/path"),
        ("publicUrl", "https://hermes.example.org?query=value"),
        ("scopes", "email profile"),
        ("clientSecret", "inline-credentials-are-unsupported"),
        ("clientSecretRef", {"name": "unsafe/name", "key": "client"}),
    ],
)
def test_invalid_oidc_inputs_fail_before_installation(
    tmp_path: Path, oidc_values: dict, key: str, value: object
) -> None:
    """Incomplete, ambiguous or unsafe OIDC settings cannot silently enable a gate."""
    oidc_values["hermes"]["dashboardAuth"]["oidc"][key] = value
    assert "schema" in render(tmp_path, oidc_values, valid=False)


@pytest.mark.parametrize("mode", ["basic", "oidc"])
@pytest.mark.parametrize("field", ["env", "secretEnv"])
@pytest.mark.parametrize(
    "name", ["HERMES_DASHBOARD_OIDC_ISSUER", "HERMES_DASHBOARD_PUBLIC_URL"]
)
def test_oidc_environment_overrides_cannot_conflict_with_chart_configuration(
    tmp_path: Path, oidc_values: dict, mode: str, field: str, name: str
) -> None:
    """The selected auth mode is authoritative for both public and secret settings."""
    oidc_values["hermes"]["dashboardAuth"]["mode"] = mode
    oidc_values["workloads"]["hermes"][field] = [
        {"name": name, **({"value": "conflict"} if field == "env" else {"key": "x"})}
    ]
    error = render(tmp_path, oidc_values, valid=False)
    assert "hermes.dashboardAuth" in error


def test_non_root_hermes_can_read_group_restricted_credentials(
    tmp_path: Path, values: dict
) -> None:
    """The setuid assistant needs its token, CA and SSH key without world access."""
    objects = render(tmp_path, values)
    pod = objects["Deployment", "hermes"]["spec"]["template"]["spec"]
    container = pod["containers"][0]
    gid = int(
        next(item["value"] for item in container["env"] if item["name"] == "HERMES_GID")
    )
    assert pod["securityContext"]["fsGroup"] == gid
    # The image still performs root initialization before dropping to Hermes.
    assert not pod["securityContext"].get("runAsNonRoot", False)
    mounts = {mount["name"]: mount for mount in container["volumeMounts"]}
    volumes = {volume["name"]: volume for volume in pod["volumes"]}
    for name in ("manager-token", "manager-ca", "controller-ssh"):
        mode = volumes[name]["secret"]["defaultMode"]
        assert mode & 0o040
        assert not mode & 0o007
        assert mounts[name]["readOnly"]


def test_image_identity_and_mount_changes_reach_manager_and_admission_together(
    tmp_path, values
):
    values["runtimeManager"].update(uid=1200, gid=1300)
    values["runtimeManager"]["config"] = {
        "templates": {
            "personal-assistant": {
                "image": IMAGE,
                "workdir_scope": {"kubernetes_claim": "custom-tasks"},
            }
        }
    }
    objects = render(tmp_path, values)
    spec = native(objects)["templates"]["personal-assistant"]["kubernetes"]
    assert spec["uid"] == 1200 and spec["gid"] == 1300
    expression = policy(objects)
    assert IMAGE in expression and 'claimName == "custom-tasks"' in expression
    assert "runAsUser == 1200" in expression and "runAsGroup == 1300" in expression
    assert values["images"]["agent-runtime"] not in expression
    # A task-only mount cannot authorize its parent PVC directory or another path.
    patterns = re.findall(r"m\.subPath\.matches\((\"(?:[^\"\\]|\\.)*\")\)", expression)
    task_patterns = [
        json.loads(pattern) for pattern in patterns if "workdir" in pattern
    ]
    assert any(
        re.fullmatch(pattern, "workspace/task/workdir") for pattern in task_patterns
    )
    assert not any(
        re.fullmatch(pattern, "workspace/../workdir") for pattern in task_patterns
    )
    assert not any(re.fullmatch(pattern, "workspace/task") for pattern in task_patterns)


@pytest.mark.parametrize(
    "mutation",
    [
        "root",
        "host-volume",
        "writable-secret",
        "missing-volume",
        "duplicate-role",
        "traversal",
        "manual-policy",
        "no-sandbox",
    ],
)
def test_unsafe_or_divergent_runtime_settings_are_rejected(tmp_path, values, mutation):
    defaults = yaml.safe_load((CHART / "values.yaml").read_text())
    template = deepcopy(
        defaults["runtimeManager"]["config"]["templates"]["personal-assistant"]
    )
    values["runtimeManager"]["config"] = {"templates": {"personal-assistant": template}}
    if mutation == "root":
        values["runtimeManager"]["uid"] = 0
    elif mutation == "host-volume":
        template["kubernetes"]["volumes"][0] = {
            "name": "ssh-public",
            "hostPath": {"path": "/"},
        }
    elif mutation == "writable-secret":
        template["kubernetes"]["volume_mounts"][0]["readOnly"] = False
    elif mutation == "missing-volume":
        template["kubernetes"]["volume_mounts"][0]["name"] = "absent"
    elif mutation == "duplicate-role":
        values["runtimeManager"]["config"]["templates"]["duplicate"] = deepcopy(
            template
        )
    elif mutation == "traversal":
        template["kubernetes"]["volume_mounts"][1]["subPath"] = "profile/../other"
    elif mutation == "manual-policy":
        values["sandbox"]["roles"] = {"attacker": {"images": ["anything"]}}
    else:
        values["sandbox"]["enabled"] = False
    render(tmp_path, values, valid=False)


def test_gateways_deny_ingress_until_an_explicit_source_is_configured(tmp_path, values):
    values["networkPolicy"] = {"namespaceIngress": {"multica-gateway": ["tailscale"]}}
    objects = render(tmp_path, values)
    assert objects["NetworkPolicy", "multica-gateway-ingress"]["spec"]["ingress"] == []
    allowed = objects["NetworkPolicy", "multica-gateway-namespace-ingress"]["spec"][
        "ingress"
    ]
    assert allowed[0]["from"] == [
        {
            "namespaceSelector": {
                "matchLabels": {"kubernetes.io/metadata.name": "tailscale"}
            }
        }
    ]


def test_only_current_manager_service_and_credential_paths_are_rendered(
    tmp_path, values
):
    objects = render(tmp_path, values)
    assert ("Service", "execution-manager") not in objects
    hermes = objects["Deployment", "hermes"]["spec"]["template"]["spec"]
    paths = {mount["mountPath"] for mount in hermes["containers"][0]["volumeMounts"]}
    assert "/etc/runtime-manager/tls" in paths
    assert "/etc/execution-manager/tls" not in paths
    auth = next(
        volume for volume in hermes["volumes"] if volume["name"] == "manager-token"
    )["secret"]
    assert auth["secretName"] == "runtime-manager-auth"
    assert {item["path"] for item in auth["items"]} == {"runtime-manager-token"}
    values["runtimeManager"]["legacyServiceAlias"] = True
    render(tmp_path, values, valid=False)


def test_native_backend_accepts_and_recognizes_chart_configured_runtime(
    tmp_path, values, monkeypatch
):
    """Use the real backend to prevent ownership, image and mount wiring drift."""
    monkeypatch.syspath_prepend(str(ROOT / "services/runtime-manager"))
    monkeypatch.delenv("KUBERNETES_SERVICE_HOST", raising=False)
    from backends import KubernetesBackend

    objects = render(tmp_path, values)
    config = native(objects)
    template = config["templates"]["personal-assistant"]
    backend = KubernetesBackend(config["kubernetes"])
    recorded = {}

    def kubectl(*args, body=None):
        if args[0] == "create":
            recorded["pod"] = deepcopy(body)
        return recorded.get("pod", "")

    monkeypatch.setattr(backend, "kubectl", kubectl)
    resource, _endpoint = backend.ensure("a" * 24, "b" * 12, template, {})
    pod = backend.get_pod(resource["name"])
    assert (
        pod["metadata"]["labels"]["app.kubernetes.io/managed-by"]
        == "agent-platform-runtime-manager"
    )
    assert pod["spec"]["containers"][0]["image"] == values["images"]["agent-runtime"]
    assert pod["spec"]["securityContext"]["runAsUser"] == 1000
    assert not pod["spec"]["automountServiceAccountToken"]
    assert pod["spec"]["volumes"] == template["kubernetes"]["volumes"]
