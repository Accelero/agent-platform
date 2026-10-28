"""Run inside the pinned Hermes image to verify the plugin and initializer."""

import importlib.util
import json
import os
import tempfile
from pathlib import Path
from unittest.mock import Mock, patch

import yaml

# Fields the execution contract accepts on acquire; the manager rejects others.
ACQUIRE_FIELDS = {
    "session",
    "mode",
    "request",
    "runtime_id",
    "parent_lease",
    "runtime",
}


def main() -> None:
    from hermes_cli import _platform_terminal_policy as policy

    # All plugin/configuration paths require the instance's managed policy,
    # including the release smoke test before any plugin is loaded.
    with tempfile.TemporaryDirectory() as directory:
        managed = Path(directory)
        (managed / "config.yaml").write_text(
            json.dumps(
                {
                    "terminal": {"backend": "managed_execution"},
                    "plugins": {"enabled": ["managed_execution"], "disabled": []},
                }
            )
        )
        with (
            patch.object(policy, "MANAGED_DIR", managed),
            patch.dict(os.environ, {"HERMES_MANAGED_DIR": str(managed)}),
        ):
            verify_plugin_and_initializer()
            managed_policy_binds_new_profiles()
    print(
        "Native Hermes plugin runtime selection, connection and initialization passed."
    )


def verify_plugin_and_initializer() -> None:
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
                "generation": "1",
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
        managed = home / "managed.json"
        connection = {
            "manager_url": "https://manager.example.org/execution",
            "token_file": str(home / "token"),
            "ca_file": str(home / "ca.crt"),
            "ssh_key": str(home / "id_ed25519"),
        }
        managed.write_text(
            json.dumps(
                {
                    "plugins": {
                        "entries": {"managed_execution": {"settings": connection}}
                    }
                }
            )
        )
        plugin.MANAGED_CONFIG = managed
        token = set_hermes_home_override(home)
        environments = []
        try:
            # Same provider/session, changing the profile's dashboard selection.
            # A cached environment must never conceal the new runtime selection.
            for runtime_id in ("python-tools", "shell-tools", "python-tools", ""):
                config = {
                    "terminal": {
                        "backend": "managed_execution",
                        "cwd": "/workspace",
                        "runtime_id": runtime_id,
                    },
                    # Profile plugin settings cannot redirect the connection.
                    "plugins": {
                        "entries": {
                            "managed_execution": {
                                "settings": {"manager_url": "https://foreign.invalid"}
                            }
                        }
                    },
                }
                (home / "config.yaml").write_text(yaml.safe_dump(config))
                before = len(calls)
                environment = provider.create_environment(
                    cwd="/workspace", timeout=10, task_id="same-session"
                )
                environments.append(environment)
                assert environment.settings["manager_url"] == connection["manager_url"]
                assert environment.settings["runtime_id"] == runtime_id
                for endpoint, payload in calls[before:]:
                    if endpoint.endswith("/acquire"):
                        assert set(payload) <= ACQUIRE_FIELDS, set(payload)
                        assert payload.get("runtime_id", "") == runtime_id
                assert (
                    provider.create_environment(
                        cwd="/workspace", timeout=10, task_id="same-session"
                    )
                    is environment
                )
            # Re-selecting a runtime reuses its transport; each selection is distinct.
            assert environments[2] is environments[0]
            assert len({id(environment) for environment in environments}) == 3
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
            # personal-init.py writes this from manager-supplied configuration.
            (home / "config.yaml").write_text(
                "terminal: {backend: managed_execution}\nmodel: user-choice\n"
            )
            saved = (home / "config.yaml").read_bytes()
            runtime.initialize(root=home)
            assert (home / "config.yaml").read_bytes() == saved
            assert (home / "plugins/hindsight").is_dir()
            # The bundled plugin serves every home; no per-home copy may shadow it.
            assert not (home / "plugins/managed_execution").exists()


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
