"""Verify native image selection, lease transitions and persisted reconciliation."""

from copy import deepcopy
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock

import pytest

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
        "deployments": {
            "fixture": {"profiles": {"default": {"templates": ["terminal"]}}}
        },
        "templates": {
            "terminal": {
                "image": DEFAULT_IMAGE,
                "allowed_images": [DEFAULT_IMAGE, CUSTOM_IMAGE],
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


@pytest.mark.parametrize(
    "image", ["unapproved:latest", "", "bad image", [CUSTOM_IMAGE]]
)
def test_invalid_or_unapproved_images_never_reach_backend(execution, image):
    from manager import Forbidden

    with pytest.raises((Forbidden, ValueError)):
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


def test_revoked_image_is_retired_without_restarting(execution):
    lease = acquire(execution, image=CUSTOM_IMAGE, mode="per_profile_always_running")
    execution.config["templates"]["terminal"]["allowed_images"] = [DEFAULT_IMAGE]
    execution.manager.maintain()
    execution.backend.remove.assert_not_called()
    execution.manager.release(lease["lease"], reconcile=False)
    execution.backend.ensure.reset_mock()
    execution.manager.maintain()
    execution.backend.remove.assert_called_once()
    execution.backend.ensure.assert_not_called()
    assert not execution.manager.status()


def test_helm_image_catalog_matches_manager_and_admission(tmp_path: Path):
    values = {
        "workloads": {"runtime-manager": {"enabled": True}},
        "runtimeManager": {
            "config": {
                "templates": {
                    "hermes": {
                        "image": DEFAULT_IMAGE,
                        "allowed_images": [DEFAULT_IMAGE, CUSTOM_IMAGE],
                    }
                }
            }
        },
    }
    objects = render(tmp_path, values)
    template = native(objects)["templates"]["hermes"]
    assert template["allowed_images"] == [DEFAULT_IMAGE, CUSTOM_IMAGE]
    expression = policy(objects)
    assert DEFAULT_IMAGE in expression and CUSTOM_IMAGE in expression
    invalid = deepcopy(values)
    invalid["runtimeManager"]["config"]["templates"]["hermes"]["allowed_images"] = [
        CUSTOM_IMAGE
    ]
    assert "include the default image" in render(tmp_path, invalid, valid=False)


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
    from manager import Forbidden, Manager

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
            assert settings["image"] in policy(objects)
            assert f'claimName == "{claim}"' in policy(objects)
            with pytest.raises(Forbidden):
                api.dispatch(
                    "hermes",
                    "/v1/leases/acquire",
                    {**payload, "image": "registry.example.org/unapproved:latest"},
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
