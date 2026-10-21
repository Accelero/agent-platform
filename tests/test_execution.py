"""Verify native image selection, lease transitions and persisted reconciliation."""

import ast
import base64
import json
from copy import deepcopy
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock

import pytest
import yaml

from agent_platform.build import ROOT
from tests.test_chart import native, policy, render

DEFAULT_IMAGE = "ghcr.io/example/terminal:default"
CUSTOM_IMAGE = "ghcr.io/example/terminal:custom"


@pytest.fixture
def execution(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    monkeypatch.syspath_prepend(str(ROOT / "services/runtime-manager"))
    from api import ExecutionAPI
    from manager import Manager

    config = {
        "clients": {"hermes": {"deployment": "fixture"}},
        "default_template": "terminal",
        "deployments": {
            "fixture": {"profiles": {"default": {"templates": ["terminal"]}}}
        },
        "templates": {
            "terminal": {
                "image": DEFAULT_IMAGE,
                "kubernetes": {"role": "hermes"},
            }
        },
    }
    backend = Mock()
    backend.ensure.side_effect = lambda name, generation, template, resource: (
        {"name": name + generation},
        {"host": "127.0.0.1", "port": 2222, "user": "worker"},
    )
    backend.remove.return_value = True
    backend.is_running.return_value = True
    manager = Manager(config, backend, str(tmp_path / "state.db"))
    api = ExecutionAPI(manager, {"hermes": "a" * 32})
    payload = {
        "profile": "default",
        "session": "session",
        "mode": "per_profile_on_demand",
        "request": "request",
    }
    try:
        yield SimpleNamespace(
            manager=manager, api=api, backend=backend, payload=payload, config=config
        )
    finally:
        manager.db.close()


def acquire(execution, **overrides: object) -> dict:
    return execution.api.dispatch(
        "hermes", "/v1/leases/acquire", {**execution.payload, **overrides}
    )


def test_plugin_defaults_to_profile_scoped_on_demand_workers():
    plugin = ROOT / "runtimes/hermes/plugins/managed_execution"
    schema = yaml.safe_load((plugin / "plugin.yaml").read_text())["config_schema"]
    source = (plugin / "__init__.py").read_text()
    defaults = next(
        node
        for node in ast.walk(ast.parse(source))
        if isinstance(node, ast.Call)
        and isinstance(node.func, ast.Name)
        and node.func.id == "dict"
        and any(keyword.arg == "mode" for keyword in node.keywords)
    )
    mode = next(k.value.value for k in defaults.keywords if k.arg == "mode")
    assert schema["mode"]["default"] == mode == "per_profile_on_demand"
    # Multica task workdirs still require their own session-scoped worker.
    assert "settings['mode'] = 'per_session'" in source


def test_profile_image_reaches_backend_and_survives_manager_restart(execution):
    from manager import Manager

    lease = acquire(execution, image=CUSTOM_IMAGE, mode="per_profile_always_running")
    assert execution.backend.ensure.call_args.args[2]["image"] == CUSTOM_IMAGE
    assert execution.config["templates"]["terminal"]["image"] == DEFAULT_IMAGE
    assert (
        acquire(execution, image=CUSTOM_IMAGE, mode="per_profile_always_running")[
            "lease"
        ]
        == lease["lease"]
    )
    execution.manager.release(lease["lease"], reconcile=False)
    execution.backend.ensure.reset_mock()
    restarted = Manager(execution.config, execution.backend, execution.manager.database)
    try:
        restarted.db.execute("UPDATE environments SET endpoint=NULL")
        restarted.db.commit()
        restarted.maintain()
        assert execution.backend.ensure.call_args.args[2]["image"] == CUSTOM_IMAGE
    finally:
        restarted.db.close()


def test_absent_profile_image_uses_deployment_default(execution):
    acquire(execution)
    assert execution.backend.ensure.call_args.args[2]["image"] == DEFAULT_IMAGE


@pytest.mark.parametrize("image", ["", "bad image", "image\x00", [CUSTOM_IMAGE]])
def test_invalid_images_never_reach_backend(execution, image):
    with pytest.raises(ValueError):
        acquire(execution, image=image)
    execution.backend.ensure.assert_not_called()


def test_image_change_drains_live_worker_and_replaces_idle_generation(execution):
    from manager import Conflict

    first = acquire(execution, image=DEFAULT_IMAGE)
    with pytest.raises(Conflict, match="image changed"):
        acquire(execution, image=CUSTOM_IMAGE, request="next")
    with pytest.raises(Conflict, match="image changed"):
        acquire(execution, image=CUSTOM_IMAGE)
    execution.manager.release(first["lease"], reconcile=False)
    second = acquire(execution, image=CUSTOM_IMAGE, request="next")
    assert second["generation"] != first["generation"]
    execution.backend.remove.assert_called_once()
    assert execution.backend.ensure.call_args.args[2]["image"] == CUSTOM_IMAGE


def test_any_profile_can_select_any_template_and_image(execution):
    execution.config["templates"]["other"] = {
        "image": "elsewhere.example/default:1",
        "kubernetes": {"role": "other"},
    }
    lease = acquire(
        execution,
        profile="new-profile",
        template="other",
        image="another-registry.example/image:2",
    )
    assert (
        execution.backend.ensure.call_args.args[2]["image"]
        == "another-registry.example/image:2"
    )
    execution.manager.release(lease["lease"], reconcile=False)
    acquire(execution, profile="second-profile", template="other", request="second")
    assert (
        execution.backend.ensure.call_args.args[2]["image"]
        == "elsewhere.example/default:1"
    )


def test_template_change_waits_for_active_work(execution):
    from manager import Conflict

    execution.config["templates"]["other"] = deepcopy(
        execution.config["templates"]["terminal"]
    )
    first = acquire(execution)
    with pytest.raises(Conflict, match="template changed"):
        acquire(execution, template="other", request="next")
    execution.manager.release(first["lease"], reconcile=False)
    second = acquire(execution, template="other", request="next")
    assert first["generation"] != second["generation"]
    assert (
        execution.manager.db.execute("SELECT template FROM environments").fetchone()[0]
        == "other"
    )


def test_helm_defaults_image_without_restricting_admission(tmp_path: Path):
    objects = render(
        tmp_path,
        {
            "workloads": {"runtime-manager": {"enabled": True}},
            "runtimeManager": {
                "config": {"templates": {"hermes": {"image": DEFAULT_IMAGE}}}
            },
        },
    )
    assert native(objects)["templates"]["hermes"]["image"] == DEFAULT_IMAGE
    assert "allowed_images" not in native(objects)["templates"]["hermes"]
    expression = policy(objects)
    assert "c.image in" not in expression
    assert DEFAULT_IMAGE not in expression
    assert "claimName" in expression


def test_registry_secrets_reach_worker_pods_without_becoming_mounted_credentials(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.syspath_prepend(str(ROOT / "services/runtime-manager"))
    monkeypatch.delenv("KUBERNETES_SERVICE_HOST", raising=False)
    from backends import KubernetesBackend

    references = [{"name": "private-registry"}, {"name": "second-registry"}]
    objects = render(
        tmp_path,
        {
            "workloads": {"runtime-manager": {"enabled": True}},
            "sandbox": {"imagePullSecrets": references},
        },
    )
    config = native(objects)
    assert config["kubernetes"]["image_pull_secrets"] == references
    assert objects["ServiceAccount", "sandbox-worker"]["imagePullSecrets"] == references
    backend = KubernetesBackend(config["kubernetes"])
    pods = {}

    def kubectl(*args, body=None):
        if args[0] == "create":
            pods["pod"] = deepcopy(body)
        return pods.get("pod", "")

    monkeypatch.setattr(backend, "kubectl", kubectl)
    backend.ensure("a" * 24, "b" * 12, config["templates"]["hermes"], None)
    pod = pods["pod"]["spec"]
    assert pod["imagePullSecrets"] == references
    assert not pod["automountServiceAccountToken"]
    assert not any(
        volume.get("secret", {}).get("secretName")
        in {item["name"] for item in references}
        for volume in pod["volumes"]
    )
    expression = policy(objects)
    assert "private-registry" in expression and "second-registry" in expression
    assert "size(object.spec.imagePullSecrets) == 2" in expression


@pytest.mark.parametrize(
    "references",
    [["inline-token"], [{"name": ""}], [{"name": "registry", "password": "inline"}]],
)
def test_registry_settings_accept_only_named_secret_references(
    tmp_path: Path, references: list
):
    render(tmp_path, {"sandbox": {"imagePullSecrets": references}}, valid=False)


def test_profile_images_registry_credentials_and_distinct_pvc_layouts_work_together(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    import yaml

    monkeypatch.syspath_prepend(str(ROOT / "services/runtime-manager"))
    monkeypatch.delenv("KUBERNETES_SERVICE_HOST", raising=False)
    from api import ExecutionAPI
    from backends import KubernetesBackend
    from manager import Manager

    values = yaml.safe_load(
        (ROOT / "examples/terminal-backends/values.yaml").read_text()
    )
    values["workloads"] = {"runtime-manager": {"enabled": True}}
    objects = render(tmp_path, values)
    config = native(objects)
    backend = KubernetesBackend(config["kubernetes"])
    pods = {}

    def kubectl(*args, body=None):
        if args[0] == "create":
            pod = deepcopy(body)
            pod["status"] = {
                "podIP": "127.0.0.1",
                "conditions": [{"type": "Ready", "status": "True"}],
            }
            pods[pod["metadata"]["name"]] = pod
            return pod
        return pods.get(args[2], "")

    monkeypatch.setattr(backend, "kubectl", kubectl)
    manager = Manager(config, backend, str(tmp_path / "manager.db"))
    api = ExecutionAPI(manager, {"hermes": "a" * 32})
    try:
        for profile, claim, path, readonly in (
            ("research", "research-data", "/datasets", True),
            ("coding", "coding-projects", "/workspace", False),
        ):
            settings = values["hermes"]["profiles"][profile]["plugins"]["entries"][
                "managed_execution"
            ]["settings"]
            payload = {
                "profile": profile,
                "session": "conversation",
                "request": profile,
                "mode": "per_session",
                "image": settings["image"],
                "template": settings["template"],
                "runtime": {"hermes_home": "/opt/data", "workdir": "/workspace"},
            }
            api.dispatch("hermes", "/v1/leases/acquire", payload)
            pod = next(
                pod
                for pod in pods.values()
                if pod["metadata"]["labels"]["hermes.stack/role"] == profile
            )
            spec = pod["spec"]
            worker = spec["containers"][0]
            assert worker["image"] == settings["image"]
            assert spec["imagePullSecrets"] == [{"name": "private-registry"}]
            volume = next(
                volume
                for volume in spec["volumes"]
                if volume.get("persistentVolumeClaim", {}).get("claimName") == claim
            )
            mount = next(
                mount
                for mount in worker["volumeMounts"]
                if mount["name"] == volume["name"]
            )
            assert mount["mountPath"] == path
            assert mount["readOnly"] is readonly
            assert mount["subPath"] == ("published" if readonly else "projects")
            assert "c.image in" not in policy(objects)
            assert f'claimName == "{claim}"' in policy(objects)
            api.dispatch(
                "hermes",
                "/v1/leases/acquire",
                {
                    **payload,
                    "session": "other",
                    "request": profile + "-other",
                    "image": "registry.example.org/another:latest",
                },
            )
    finally:
        manager.db.close()


@pytest.mark.parametrize(
    "overrides",
    [
        {"namespace": "other-namespace"},
        {"volumes": [{"persistentVolumeClaim": {"claimName": "private-data"}}]},
        {"pvc": "private-data"},
        {"template": "unapproved"},
        {"runtime": {"workdir": "/workspace", "pvc": "private-data"}},
    ],
)
def test_hermes_cannot_introduce_namespaces_claims_or_mount_specs(execution, overrides):
    if "runtime" in overrides:
        execution.config["templates"]["terminal"]["workdir_scope"] = {
            "controller_root": "/opt/data/.multica/workspaces"
        }
    with pytest.raises(ValueError):
        acquire(execution, **overrides)
    execution.backend.ensure.assert_not_called()


def test_manager_authority_is_limited_to_pods_in_platform_execution_namespace(
    tmp_path: Path,
):
    namespace = "platform-execution"
    objects = render(
        tmp_path,
        {
            "workloads": {"runtime-manager": {"enabled": True}},
            "sandbox": {"namespace": namespace},
        },
    )
    role = objects["Role", "runtime-manager"]
    assert role["metadata"]["namespace"] == namespace
    assert role["rules"] == [
        {"apiGroups": [""], "resources": ["pods"], "verbs": ["get", "create", "delete"]}
    ]
    binding = objects["RoleBinding", "runtime-manager"]
    assert binding["metadata"]["namespace"] == namespace
    assert binding["roleRef"]["kind"] == "Role"
    assert binding["subjects"] == [
        {"kind": "ServiceAccount", "name": "runtime-manager", "namespace": "agents"}
    ]
    assert not any(kind in {"ClusterRole", "ClusterRoleBinding"} for kind, _ in objects)
    assert native(objects)["kubernetes"]["namespace"] == namespace


def test_execution_namespace_defaults_to_the_application_and_cannot_share_it(
    tmp_path: Path,
):
    objects = render(tmp_path, {"workloads": {"runtime-manager": {"enabled": True}}})
    assert native(objects)["kubernetes"]["namespace"] == "agents-execution"
    assert objects["Namespace", "agents-execution"]
    assert "dedicated execution namespace" in render(
        tmp_path, {"sandbox": {"namespace": "agents"}}, valid=False
    )


def test_helm_registry_objects_generate_one_secret_for_all_worker_images(
    tmp_path: Path,
):
    objects = render(
        tmp_path,
        {
            "workloads": {"runtime-manager": {"enabled": True}},
            "sandbox": {
                "imagePullSecrets": [{"name": "external-registry"}],
                "registryCredentials": {
                    "ghcr.io": {"username": "robot", "password": "fixture-ghcr-token"},
                    "registry.example.org:5000": {
                        "username": "other",
                        "password": "fixture-other-token",
                    },
                },
            },
        },
    )
    secret = objects["Secret", "agents-terminal-registry"]
    assert secret["metadata"]["namespace"] == "agents-execution"
    assert secret["type"] == "kubernetes.io/dockerconfigjson"
    auths = json.loads(base64.b64decode(secret["data"][".dockerconfigjson"]))["auths"]
    assert base64.b64decode(auths["ghcr.io"]["auth"]) == b"robot:fixture-ghcr-token"
    assert (
        base64.b64decode(auths["registry.example.org:5000"]["auth"])
        == b"other:fixture-other-token"
    )
    references = [{"name": "external-registry"}, {"name": "agents-terminal-registry"}]
    assert native(objects)["kubernetes"]["image_pull_secrets"] == references
    assert objects["ServiceAccount", "sandbox-worker"]["imagePullSecrets"] == references
    assert "size(object.spec.imagePullSecrets) == 2" in policy(objects)
    assert "fixture-ghcr-token" not in json.dumps(native(objects))
    assert "fixture-other-token" not in policy(objects)
    manager = objects["Deployment", "runtime-manager"]
    assert "agents-terminal-registry" not in json.dumps(manager)


@pytest.mark.parametrize(
    "credentials",
    [
        ["one-token"],
        {"ghcr.io": {"password": "token"}},
        {"ghcr.io": {"username": "robot", "password": ""}},
        {"ghcr.io": {"username": "robot:bad", "password": "token"}},
        {"ghcr.io": {"username": "robot", "password": "token", "extra": "invalid"}},
    ],
)
def test_registry_object_requires_registry_specific_complete_logins(
    tmp_path: Path, credentials
):
    render(tmp_path, {"sandbox": {"registryCredentials": credentials}}, valid=False)


def test_default_template_must_exist(tmp_path: Path):
    error = render(
        tmp_path,
        {"runtimeManager": {"config": {"default_template": "missing"}}},
        valid=False,
    )
    assert "must name a configured template" in error


def test_per_profile_workspace_mapping_mounts_only_the_selected_profile(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.syspath_prepend(str(ROOT / "services/runtime-manager"))
    monkeypatch.delenv("KUBERNETES_SERVICE_HOST", raising=False)
    from api import ExecutionAPI
    from backends import KubernetesBackend
    from manager import Manager

    validation = tmp_path / "profile-workspaces"
    for profile in ("research", "coding"):
        (validation / profile).mkdir(parents=True)
    values = {
        "workloads": {"runtime-manager": {"enabled": True}},
        "runtimeManager": {
            "config": {
                "templates": {
                    "hermes": {
                        "profile_workspace": {
                            "kubernetes_claim": "hermes-workspaces",
                            "kubernetes_subpath": "{profile}",
                            "mount_path": "/workspace",
                            "controller_root": "/opt/data/workspaces/{profile}",
                            "validation_root": str(validation),
                        }
                    }
                }
            }
        },
    }
    objects = render(tmp_path, values)
    config = native(objects)
    rules = policy(objects)
    assert "[A-Za-z0-9][A-Za-z0-9_-]{0,63}" in rules
    assert "{profile}" not in rules
    backend = KubernetesBackend(config["kubernetes"])
    pods = {}

    def kubectl(*args, body=None):
        if args[0] == "create":
            pod = deepcopy(body)
            pod["status"] = {
                "podIP": "127.0.0.1",
                "conditions": [{"type": "Ready", "status": "True"}],
            }
            pods[pod["metadata"]["name"]] = pod
            return pod
        return pods.get(args[2], "")

    monkeypatch.setattr(backend, "kubectl", kubectl)
    manager = Manager(config, backend, str(tmp_path / "manager.db"))
    api = ExecutionAPI(manager, {"hermes": "a" * 32})
    try:
        subpaths = {}
        for profile in ("research", "coding"):
            before = set(pods)
            api.dispatch(
                "hermes",
                "/v1/leases/acquire",
                {
                    "profile": profile,
                    "session": "conversation",
                    "request": profile,
                    "mode": "per_profile_on_demand",
                    "runtime": {
                        "hermes_home": "/opt/data/profiles/" + profile,
                        "workdir": "/workspace",
                    },
                },
            )
            (name,) = set(pods) - before
            spec = pods[name]["spec"]
            volume = next(
                volume
                for volume in spec["volumes"]
                if volume.get("persistentVolumeClaim", {}).get("claimName")
                == "hermes-workspaces"
            )
            (mount,) = [
                mount
                for mount in spec["containers"][0]["volumeMounts"]
                if mount["name"] == volume["name"]
            ]
            assert mount["mountPath"] == "/workspace"
            subpaths[profile] = mount["subPath"]
        assert subpaths == {"research": "research", "coding": "coding"}
        with pytest.raises(ValueError, match="missing"):
            api.dispatch(
                "hermes",
                "/v1/leases/acquire",
                {
                    "profile": "unprepared",
                    "session": "conversation",
                    "request": "unprepared",
                    "mode": "per_profile_on_demand",
                    "runtime": {
                        "hermes_home": "/opt/data/profiles/unprepared",
                        "workdir": "/workspace",
                    },
                },
            )
    finally:
        manager.db.close()
