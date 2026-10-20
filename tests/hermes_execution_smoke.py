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
            "/opt/hermes-stack/plugin-home/plugins/managed_execution",
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
            for image in ("example.org/research:1", "example.org/coding:2", ""):
                config = {
                    "terminal": {"backend": "managed_execution", "cwd": "/workspace"},
                    "plugins": {
                        "entries": {
                            "managed_execution": {
                                "settings": {"image": image, "profile": "default"}
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
            runtime.initialize(root=home)
            config = yaml.safe_load((home / "config.yaml").read_text())
            assert config["terminal"]["backend"] == "managed_execution"
            assert "model" not in config
            assert not (home / "profiles").exists()
            assert (home / "plugins/managed_execution/plugin.yaml").is_file()
            (home / "config.yaml").write_text(
                "terminal: {backend: managed_execution}\nmodel: user-choice\n"
            )
            saved = (home / "config.yaml").read_bytes()
            runtime.initialize(root=home)
            assert (home / "config.yaml").read_bytes() == saved
    print("Native Hermes plugin image selection, caching and initialization passed.")


if __name__ == "__main__":
    main()
