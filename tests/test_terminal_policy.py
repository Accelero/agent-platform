"""Personal terminal policy fails closed and reserves the image-owned provider."""

import importlib.util
import json
import sys
from pathlib import Path
from types import ModuleType, SimpleNamespace

import pytest


@pytest.fixture
def policy(tmp_path, monkeypatch):
    source = Path(__file__).resolve().parents[1] / "runtimes/hermes/terminal-policy.py"
    spec = importlib.util.spec_from_file_location("terminal_policy", source)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    monkeypatch.setattr(module, "OWNER_CONTEXT", tmp_path / "owner.json")
    monkeypatch.setattr(module, "MANAGED_DIR", tmp_path)
    monkeypatch.setattr(module, "PLUGIN_DIR", tmp_path / "image-plugin")
    monkeypatch.delenv("HERMES_PERSONAL_INSTANCE_ID", raising=False)
    return module


def enable(policy):
    policy.OWNER_CONTEXT.write_text("{}")
    config = {
        "terminal": {"backend": "managed_execution"},
        "plugins": {"enabled": ["managed_execution"], "disabled": []},
    }
    (policy.MANAGED_DIR / "config.yaml").write_text(json.dumps(config))
    return config


def test_legacy_policy_preserves_upstream_backend_and_discovery(policy):
    policy.enforce_backend("local")
    manifests = [SimpleNamespace(name="managed_execution", source="user")]
    assert policy.trusted_manifests(manifests) is manifests
    assert policy.managed_directory() is None


@pytest.mark.parametrize(
    "content",
    [None, "secret malformed config", "[]", "{}", '"secret"', "x" * 16385],
)
def test_missing_or_invalid_policy_fails_closed_without_config_in_error(
    policy, content
):
    policy.OWNER_CONTEXT.touch()
    if content is not None:
        (policy.MANAGED_DIR / "config.yaml").write_text(content)
    with pytest.raises(
        RuntimeError, match="^Personal terminal policy is missing or invalid$"
    ):
        policy.enforce_backend("local")


@pytest.mark.parametrize(
    "field,value", [("enabled", []), ("disabled", ["managed_execution"])]
)
def test_policy_must_enable_required_plugin(policy, field, value):
    config = enable(policy)
    config["plugins"][field] = value
    (policy.MANAGED_DIR / "config.yaml").write_text(json.dumps(config))
    with pytest.raises(RuntimeError):
        policy.load_policy()


def test_owner_mount_pins_policy_despite_changed_environment(policy, monkeypatch):
    enable(policy)
    monkeypatch.setenv("HERMES_MANAGED_DIR", "/tmp/untrusted")
    monkeypatch.setenv("HERMES_PERSONAL_INSTANCE_ID", "")
    assert policy.managed_directory() == policy.MANAGED_DIR
    policy.enforce_backend("managed_execution")
    for backend in ["local", "ssh", "docker", "other-plugin"]:
        with pytest.raises(ValueError, match="require the managed_execution"):
            policy.enforce_backend(backend)
    config_module = ModuleType("hermes_cli.config")
    config_module.TERMINAL_CONFIG_ENV_MAP = {"backend": "TERMINAL_ENV"}
    config_module._terminal_env_value = str
    monkeypatch.setitem(sys.modules, "hermes_cli.config", config_module)
    assert policy.terminal_override("TERMINAL_ENV") == "managed_execution"
    assert policy.terminal_override("TERMINAL_RUNTIME_ID") is None


def test_image_plugin_cannot_be_shadowed_or_disabled(policy):
    enable(policy)
    pinned = SimpleNamespace(
        name="managed_execution",
        key="managed_execution",
        source="bundled",
        path=policy.PLUGIN_DIR,
    )
    other = SimpleNamespace(name="other", key="other", source="user", path="other")
    shadows = [
        SimpleNamespace(
            name=name, key=key, source=source, path="/tmp/managed_execution"
        )
        for name, key, source in [
            ("managed_execution", "managed_execution", "user"),
            ("managed_execution", "category/provider", "project"),
            ("impostor", "managed_execution", "entrypoint"),
        ]
    ]
    assert policy.trusted_manifests([pinned, other, *shadows]) == [other, pinned]
    assert policy.required_manifest(pinned)
    assert not policy.required_manifest(other)
    for shadow in shadows:
        with pytest.raises(RuntimeError, match="must come from the image"):
            policy.required_manifest(shadow)
    with pytest.raises(RuntimeError, match="unavailable"):
        policy.trusted_manifests(shadows)
