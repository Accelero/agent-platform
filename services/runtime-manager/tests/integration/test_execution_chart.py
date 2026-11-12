"""The chart renders the manager's configuration, RBAC and admission, and the manager
accepts exactly that configuration (R7, R8, R9, X11)."""

import base64
import json
from pathlib import Path

import pytest
import yaml
from runtime_manager.domain.model import pod_name

from agent_platform.build import ROOT
from tests.support.chart import native, policy, render
from tests.support.world import make_world

DEFAULT_IMAGE = "ghcr.io/example/terminal:default"


def manifests(world):
    return {name: pod["manifest"] for name, pod in world.kube.pods.items()}


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


@pytest.mark.parametrize(
    "references",
    [["inline-token"], [{"name": ""}], [{"name": "registry", "password": "inline"}]],
)
def test_registry_settings_accept_only_named_secret_references(
    tmp_path: Path, references: list
):
    render(tmp_path, {"sandbox": {"imagePullSecrets": references}}, valid=False)


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
        {
            "apiGroups": [""],
            "resources": ["pods"],
            "verbs": ["get", "list", "create", "delete"],
        }
    ]
    binding = objects["RoleBinding", "runtime-manager"]
    assert binding["metadata"]["namespace"] == namespace
    assert binding["roleRef"]["kind"] == "Role"
    assert binding["subjects"] == [
        {"kind": "ServiceAccount", "name": "runtime-manager", "namespace": "agents"}
    ]
    assert not any(kind in {"ClusterRole", "ClusterRoleBinding"} for kind, _ in objects)
    assert native(objects)["kubernetes"]["namespace"] == namespace


def test_registry_secrets_reach_worker_pods_without_becoming_mounted_credentials(
    tmp_path: Path,
):
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
    world = make_world(tmp_path, config=config)
    try:
        name = pod_name("a" * 24, "b" * 12)
        world.cluster.create(name, "a" * 24, config["templates"]["hermes"])
        pod = manifests(world)[name]["spec"]
    finally:
        world.close()
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


def test_profile_images_registry_credentials_and_distinct_pvc_layouts_work_together(
    tmp_path: Path,
):
    values = yaml.safe_load(
        (ROOT / "examples/terminal-backends/values.yaml").read_text()
    )
    values["workloads"] = {"runtime-manager": {"enabled": True}}
    objects = render(tmp_path, values)
    world = make_world(tmp_path, config=native(objects))
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
            world.acquire(**payload)
            spec = next(
                pod["spec"]
                for pod in manifests(world).values()
                if pod["metadata"]["labels"]["hermes.stack/role"] == profile
            )
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
            world.acquire(
                **{
                    **payload,
                    "session": "other",
                    "request": profile + "-other",
                    "image": "registry.example.org/another:latest",
                }
            )
    finally:
        world.close()


def test_per_profile_workspace_mapping_mounts_only_the_selected_profile(
    tmp_path: Path,
):
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
    rules = policy(objects)
    assert "[A-Za-z0-9][A-Za-z0-9_-]{0,63}" in rules
    assert "{profile}" not in rules
    world = make_world(tmp_path, config=native(objects))

    def runtime(profile):
        return {"hermes_home": "/opt/data/profiles/" + profile, "workdir": "/workspace"}

    try:
        subpaths = {}
        for profile in ("research", "coding"):
            before = set(world.kube.pods)
            world.acquire(profile=profile, request=profile, runtime=runtime(profile))
            (name,) = set(world.kube.pods) - before
            spec = manifests(world)[name]["spec"]
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
            world.acquire(
                profile="unprepared",
                request="unprepared",
                runtime=runtime("unprepared"),
            )
    finally:
        world.close()


def test_worker_host_key_reaches_workers_and_only_the_public_half_reaches_hermes(
    tmp_path: Path,
):
    """X14: workers mount the private key, Hermes only the public key, admission
    allows both.
    """
    enabled = {"runtime-manager": {"enabled": True}, "hermes": {"enabled": True}}
    plain = render(tmp_path, {"workloads": enabled})
    assert "ssh-host" not in json.dumps(native(plain))
    assert "worker-host-key" not in json.dumps(plain["Deployment", "hermes"])
    objects = render(
        tmp_path,
        {
            "workloads": enabled,
            "sandbox": {"hostKey": {"secretName": "worker-host"}},
        },
    )
    template = native(objects)["templates"]["hermes"]["kubernetes"]
    (volume,) = [v for v in template["volumes"] if v["name"] == "ssh-host"]
    assert volume["secret"]["secretName"] == "worker-host"
    assert volume["secret"]["items"] == [
        {"key": "ssh_host_ed25519_key", "path": "ssh_host_ed25519_key"}
    ]
    (mount,) = [m for m in template["volume_mounts"] if m["name"] == "ssh-host"]
    assert mount == {
        "name": "ssh-host",
        "mountPath": "/etc/ssh/hostkeys/ssh_host_ed25519_key",
        "subPath": "ssh_host_ed25519_key",
        "readOnly": True,
    }
    assert "worker-host" in policy(objects)
    hermes = objects["Deployment", "hermes"]["spec"]["template"]["spec"]
    (shared,) = [v for v in hermes["volumes"] if v["name"] == "worker-host-key"]
    assert shared["secret"]["items"] == [
        {"key": "ssh_host_ed25519_key.pub", "path": "host-key.pub"}
    ]
    mounts = hermes["containers"][0]["volumeMounts"]
    assert {
        "name": "worker-host-key",
        "mountPath": "/opt/ssh-host",
        "readOnly": True,
    } in mounts
