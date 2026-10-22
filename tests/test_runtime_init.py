"""Exercise generic native configuration initialization and state preservation."""

import importlib.util
import json
from pathlib import Path

import pytest
import yaml

from agent_platform.build import ROOT
from tests.test_chart import render


@pytest.fixture
def initializer(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.syspath_prepend(str(ROOT / "runtimes/hermes"))
    spec = importlib.util.spec_from_file_location(
        "runtime_init", ROOT / "runtimes/hermes/runtime-init.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_helm_profiles_seed_independent_images_without_overwriting_live_settings(
    tmp_path: Path, initializer
):
    values = {
        "workloads": {"hermes": {"enabled": True}},
        "hermes": {
            "profiles": {
                "research": {
                    "plugins": {
                        "entries": {
                            "managed_execution": {
                                "settings": {"image": "example.org/research:1"}
                            }
                        }
                    }
                },
                "coding": {
                    "plugins": {
                        "entries": {
                            "managed_execution": {
                                "settings": {"image": "example.org/coding:1"}
                            }
                        }
                    }
                },
            }
        },
    }
    objects = render(tmp_path, values)
    defaults = json.loads(
        objects["ConfigMap", "hermes-defaults"]["data"]["defaults.json"]
    )
    home = tmp_path / "home"
    initializer.seed_defaults(home, defaults)
    root = yaml.safe_load((home / "config.yaml").read_text())
    assert root["plugins"]["entries"]["managed_execution"]["settings"] == {
        "profile": "default"
    }
    for name in ("research", "coding"):
        path = home / "profiles" / name / "config.yaml"
        config = yaml.safe_load(path.read_text())
        assert config["terminal"]["backend"] == "managed_execution"
        settings = config["plugins"]["entries"]["managed_execution"]["settings"]
        assert settings == {"profile": name, "image": f"example.org/{name}:1"}
    path.write_text("model: user-choice\nterminal: {backend: managed_execution}\n")
    initializer.seed_defaults(home, defaults)
    assert path.read_text() == (
        "model: user-choice\nterminal: {backend: managed_execution}\n"
    )
    pod = objects["Deployment", "hermes"]["spec"]["template"]["spec"]
    mount = next(
        item
        for item in pod["containers"][0]["volumeMounts"]
        if item["name"] == "hermes-defaults"
    )
    assert mount["readOnly"] is True


@pytest.mark.parametrize("name", ["../escape", ".", "default", "custom", "a/b"])
def test_profile_defaults_cannot_write_outside_native_home(
    tmp_path: Path, initializer, name: str
):
    with pytest.raises(ValueError, match="profile"):
        initializer.seed_defaults(tmp_path, {"config": {}, "profiles": {name: {}}})
    assert not list(tmp_path.iterdir())


def test_config_seed_refuses_symlinked_parent(tmp_path: Path, initializer):
    outside = tmp_path / "outside"
    outside.mkdir()
    home = tmp_path / "home"
    home.mkdir()
    (home / "profiles").symlink_to(outside, target_is_directory=True)
    with pytest.raises(RuntimeError, match="initialize"):
        initializer.seed_defaults(home, {"config": {}, "profiles": {"research": {}}})
    assert not list(outside.iterdir())


def test_default_chart_has_no_named_persona_or_native_home_worker_mount(tmp_path: Path):
    objects = render(
        tmp_path,
        {
            "workloads": {
                "hermes": {"enabled": True},
                "runtime-manager": {"enabled": True},
            }
        },
    )
    defaults = json.loads(
        objects["ConfigMap", "hermes-defaults"]["data"]["defaults.json"]
    )
    assert defaults["profiles"] == {}
    manager = yaml.safe_load(
        objects["ConfigMap", "runtime-manager-config"]["data"]["config.yaml"]
    )
    template = manager["templates"]["hermes"]
    mounts = template["kubernetes"]["volume_mounts"]
    assert {item["mountPath"] for item in mounts} == {
        "/workspace",
        "/etc/ssh/authorized_keys/worker",
    }
    assert "identity_kubernetes_claim" not in template["workdir_scope"]


def _adapter_home(tmp_path: Path) -> Path:
    plugin_home = tmp_path / "plugin-home"
    (plugin_home / "plugins/hindsight").mkdir(parents=True)
    (plugin_home / "plugins/hindsight/plugin.yaml").write_text("name: hindsight\n")
    return plugin_home


def test_managed_backend_covers_profiles_saved_without_terminal_backend(
    tmp_path: Path, initializer
):
    home = tmp_path / "home"
    profile = home / "profiles/research"
    legacy = profile / "plugins/managed_execution"
    legacy.mkdir(parents=True)
    (legacy / "plugin.yaml").write_text("name: managed_execution\n")
    # Native dashboard saves drop keys that the managed layer already supplies.
    (profile / "config.yaml").write_text("terminal: {cwd: /workspace}\nmodel: chosen\n")
    managed = tmp_path / "managed.yaml"
    managed.write_text("terminal: {backend: managed_execution}\n")
    initializer.initialize(
        root=home, plugin_home=_adapter_home(tmp_path), managed=managed
    )
    assert not legacy.exists()
    assert (profile / "plugins/hindsight/plugin.yaml").is_file()
    assert (home / "workspaces/research").is_dir()
    assert "model: chosen" in (profile / "config.yaml").read_text()


def test_profile_without_managed_backend_policy_is_refused(tmp_path: Path, initializer):
    home = tmp_path / "home"
    profile = home / "profiles/research"
    profile.mkdir(parents=True)
    (profile / "config.yaml").write_text("terminal: {backend: local}\n")
    with pytest.raises(RuntimeError, match="Unsafe native terminal backend"):
        initializer.initialize(
            root=home,
            plugin_home=_adapter_home(tmp_path),
            managed=tmp_path / "absent.yaml",
        )
    assert not (profile / "plugins").exists()
