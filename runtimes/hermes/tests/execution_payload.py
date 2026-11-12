"""Run inside the pinned Hermes image to verify its native plugin configuration API."""

import importlib.util
import os
import tempfile
from pathlib import Path
from unittest.mock import Mock, patch

import yaml


def main() -> None:
    from hermes_cli.plugins import PluginContext, PluginManifest
    from hermes_constants import (
        reset_hermes_home_override,
        set_hermes_home_override,
    )

    path = Path(
        os.environ.get(
            "MANAGED_EXECUTION_PLUGIN",
            "/opt/hermes/plugins/managed_execution",
        )
    )
    spec = importlib.util.spec_from_file_location(
        "managed_execution", path / "__init__.py"
    )
    plugin = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(plugin)
    ctx = PluginContext(PluginManifest(name="managed_execution"), Mock())
    provider = plugin.Provider(ctx)
    calls = []

    def call(client, endpoint: str, payload=None, **kwargs) -> dict:
        calls.append((endpoint, payload))
        if endpoint.endswith("/acquire"):
            return {
                "lease": "lease",
                "generation": "generation",
                "environment": "environment",
                "ttl": 120,
                "ssh": {"host": "127.0.0.1", "port": 2222, "user": "worker"},
            }
        return {}

    with (
        tempfile.TemporaryDirectory() as directory,
        patch.object(plugin.Client, "call", call),
        patch.object(plugin.ManagedSSHEnvironment, "_connect"),
    ):
        home = Path(directory)
        token = set_hermes_home_override(home)
        environments = []
        try:
            # Same provider/session, changing native profile settings. A cached
            # environment must never conceal the new image selection.
            for template, image in (
                ("research", "example.org/research:1"),
                ("coding", "example.org/coding:2"),
                ("research", ""),
                ("coding", ""),
            ):
                config = {
                    "terminal": {"backend": "managed_execution", "cwd": "/workspace"},
                    "plugins": {
                        "entries": {
                            "managed_execution": {
                                "settings": {
                                    "image": image,
                                    "template": template,
                                    "profile": "default",
                                }
                            }
                        }
                    },
                }
                (home / "config.yaml").write_text(yaml.safe_dump(config))
                environment = provider.create_environment(
                    cwd="/workspace", timeout=10, task_id="same-session"
                )
                environments.append(environment)
                payload = next(
                    payload
                    for endpoint, payload in reversed(calls)
                    if endpoint.endswith("/acquire")
                )
                assert payload.get("image", "") == image
                assert payload["profile"] == "default"
                assert payload["template"] == template
                assert (
                    not image
                    or payload["image"] != "nikolaik/python-nodejs:python3.11-nodejs20"
                )
                assert (
                    provider.create_environment(
                        cwd="/workspace", timeout=10, task_id="same-session"
                    )
                    is environment
                )
            assert len({id(environment) for environment in environments}) == 4
        finally:
            for environment in environments:
                environment.cleanup()
            reset_hermes_home_override(token)
    initializer = Path("/usr/local/lib/runtime-init.py")
    if initializer.is_file():
        import sys

        sys.path.insert(0, str(initializer.parent))
        spec = importlib.util.spec_from_file_location("runtime_init", initializer)
        runtime = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(runtime)
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            runtime.initialize(root=home)
            config = yaml.safe_load((home / "config.yaml").read_text())
            assert config["terminal"]["backend"] == "managed_execution"
            assert "model" not in config
            assert not (home / "profiles").exists()
            # The bundled plugin serves every home; no per-home copy may shadow it.
            assert not (home / "plugins/managed_execution").exists()
            (home / "config.yaml").write_text(
                "terminal: {backend: managed_execution}\nmodel: user-choice\n"
            )
            saved = (home / "config.yaml").read_bytes()
            runtime.initialize(root=home)
            assert (home / "config.yaml").read_bytes() == saved
    managed_policy_binds_new_profiles()
    multica_task_context_reaches_every_profile(plugin, provider)
    print("Native Hermes plugin image selection, caching and initialization passed.")


def multica_task_context_reaches_every_profile(plugin, provider) -> None:
    """A Multica task forwards its task variables without profile configuration."""
    from tools.env_passthrough import get_all_passthrough

    def call(client, endpoint: str, payload=None, **kwargs) -> dict:
        if endpoint.endswith("/acquire"):
            return {
                "lease": "lease",
                "generation": "generation",
                "environment": "environment",
                "ttl": 120,
                "ssh": {"host": "127.0.0.1", "port": 2222, "user": "worker"},
            }
        return {}

    with (
        patch.object(plugin.Client, "call", call),
        patch.object(plugin.ManagedSSHEnvironment, "_connect"),
        patch.object(plugin, "multica_workdir", return_value="/tmp/task/workdir"),
    ):
        environment = provider.create_environment(
            cwd="/workspace", timeout=10, task_id="multica-task"
        )
        try:
            assert set(plugin.MULTICA_TASK_VARIABLES) <= get_all_passthrough()
        finally:
            environment.cleanup()


def managed_policy_binds_new_profiles() -> None:
    """A profile created after startup, without terminal settings or a plugin copy,
    must still resolve to managed SSH execution and never to local commands."""
    from hermes_cli.plugins import discover_plugins, get_plugin_manager
    from hermes_constants import reset_hermes_home_override, set_hermes_home_override
    from tools.terminal_scope import build_profile_terminal_scope
    from tools.terminal_tool_backends import _get_plugin_env_provider

    with tempfile.TemporaryDirectory() as directory:
        home = Path(directory) / "profiles" / "created-later"
        home.mkdir(parents=True)
        (home / "config.yaml").write_text("model:\n  default: chosen\n")
        (home / ".env").write_text("TERMINAL_ENV=local\n")
        scope = build_profile_terminal_scope(home)
        assert scope["TERMINAL_ENV"] == "managed_execution", scope.get("TERMINAL_ENV")
        token = set_hermes_home_override(home)
        try:
            discover_plugins(force=True)
            plugin = get_plugin_manager()._plugins["managed_execution"]
            assert plugin.enabled and plugin.manifest.source == "bundled"
            assert _get_plugin_env_provider("managed_execution") is not None
        finally:
            reset_hermes_home_override(token)


if __name__ == "__main__":
    main()
