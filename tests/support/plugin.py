"""Load the shipped plugin with only its Hermes transport boundary stubbed."""

import importlib.util
import sys
import types
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "runtimes/hermes/plugins/managed_execution/__init__.py"


class SSHStub:
    def _target_flags(self, port_flag):
        return []

    def _build_ssh_command(self, extra_args=None, send_env=()):
        return [
            "ssh",
            "-o",
            "StrictHostKeyChecking=accept-new",
            *self._target_flags("-p"),
        ]


def load_plugin(monkeypatch):
    provider = types.ModuleType("agent.terminal_env_provider")
    provider.TerminalEnvironmentProvider = type("TerminalEnvironmentProvider", (), {})
    ssh = types.ModuleType("tools.environments.ssh")
    ssh.SSHEnvironment = SSHStub
    monkeypatch.setitem(sys.modules, provider.__name__, provider)
    monkeypatch.setitem(sys.modules, ssh.__name__, ssh)
    spec = importlib.util.spec_from_file_location("managed_execution_test", SOURCE)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def client_settings(served, tmp_path, **overrides):
    token = tmp_path / "token"
    token.write_text(served.token)
    return {
        "manager_url": f"https://localhost:{served.port}",
        "ca_file": str(served.certificate),
        "token_file": str(token),
        "profile": "default",
        "mode": "per_profile_on_demand",
        "acquire_timeout_seconds": 1,
        "acquire_retry_seconds": 0.01,
        **overrides,
    }


class Ticks:
    """Exercise a heartbeat loop synchronously, without waiting for real TTLs."""

    def __init__(self, callback, limit=20):
        self.callback, self.limit, self.count = callback, limit, 0

    def wait(self, seconds):
        self.count += 1
        if self.count > self.limit:
            return True
        self.callback(seconds)
        return False
