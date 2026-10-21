"""Native protocol retries and cache isolation for approved runtime selection."""

import importlib.util
import sys
import threading
import types
import urllib.error
from pathlib import Path
from types import SimpleNamespace

import pytest

SOURCE = (
    Path(__file__).resolve().parents[1]
    / "runtimes/hermes/plugins/managed_execution/__init__.py"
)


@pytest.fixture
def plugin(monkeypatch):
    # These tests use the native provider's public factory without starting SSH.
    # The real pinned Hermes image is covered by personal_hermes/native_runtime.py.
    dependencies = {
        "agent.terminal_env_provider": {"TerminalEnvironmentProvider": object},
        "tools.environments.ssh": {"SSHEnvironment": object},
        "gateway.session_context": {"get_session_env": lambda key, default: default},
        "hermes_cli.config": {"get_hermes_home": lambda: Path("/opt/data")},
    }
    for name, values in dependencies.items():
        module = types.ModuleType(name)
        module.__dict__.update(values)
        monkeypatch.setitem(sys.modules, name, module)
    spec = importlib.util.spec_from_file_location("managed_execution_selection", SOURCE)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def settings(**changes):
    return {
        "profile": "default",
        "mode": "per_session",
        "acquire_timeout_seconds": 30,
        **changes,
    }


def test_acquire_retry_preserves_selected_runtime_and_request_identity(
    plugin, monkeypatch
):
    client = plugin.Client(settings(runtime_id="python-tools"))
    requests = []

    def call(path, body, timeout):
        requests.append((path, dict(body)))
        if len(requests) == 1:
            raise urllib.error.HTTPError("https://fixture", 409, "pending", {}, None)
        return {"lease": "committed"}

    monkeypatch.setattr(client, "call", call)
    monkeypatch.setattr(plugin.time, "sleep", lambda delay: None)
    assert client.acquire("conversation", parent_lease="parent") == {
        "lease": "committed"
    }
    assert len(requests) == 2
    assert requests[0] == requests[1]
    assert requests[0][1]["runtime_id"] == "python-tools"
    assert requests[0][1]["parent_lease"] == "parent"
    assert requests[0][1]["request"]


def test_absent_selector_keeps_legacy_request_compatible(plugin, monkeypatch):
    client = plugin.Client(settings())
    monkeypatch.setattr(client, "call", lambda path, body, timeout: body)
    assert "runtime_id" not in client.acquire("conversation")


def test_factory_cache_separates_runtime_and_session_close_targets_selection(
    plugin, monkeypatch
):
    config = {"profile": "default", "runtime_id": "python-tools"}
    context = SimpleNamespace(
        get_config=lambda key, default: config.get(key, default), profile_name="default"
    )
    provider = plugin.Provider(context)
    monkeypatch.setattr(plugin, "invocation_cwd", lambda cwd, task: cwd)

    class Environment:
        def __init__(self, *args, **kwargs):
            self._closed = False

    monkeypatch.setattr(plugin, "ManagedSSHEnvironment", Environment)

    def create():
        return provider.create_environment(
            cwd="/workspace", timeout=30, task_id="conversation"
        )

    first = create()
    assert create() is first
    config["runtime_id"] = "shell-tools"
    second = create()
    assert second is not first
    config["runtime_id"] = "python-tools"
    assert create() is first
    config["runtime_id"] = "owner/image:latest"
    with pytest.raises(ValueError):
        create()


def test_session_close_preserves_selected_runtime_context(plugin):
    environment = plugin.ManagedSSHEnvironment.__new__(plugin.ManagedSSHEnvironment)
    environment._guard = threading.RLock()
    environment._closed = False
    environment._reply = None
    environment.settings = settings(runtime_id="python-tools")
    environment.session = "conversation"
    environment.runtime = {"hermes_home": "/opt/data", "workdir": "/workspace"}
    requests = []
    environment.client = SimpleNamespace(
        call=lambda path, body, timeout: requests.append((path, body))
    )
    environment.provider = SimpleNamespace(
        _guard=threading.RLock(), _environments=set()
    )
    environment.cleanup()
    environment.cleanup()
    assert requests == [
        (
            "/v1/sessions/close",
            {
                "profile": "default",
                "session": "conversation",
                "runtime": environment.runtime,
                "runtime_id": "python-tools",
            },
        )
    ]


def test_dashboard_profile_selection_overrides_plugin_default_without_changing_scope(
    plugin, monkeypatch
):
    config = sys.modules["hermes_cli.config"]

    current = {"terminal": {"runtime_id": "python-tools"}}
    monkeypatch.setattr(config, "load_config", lambda: current, raising=False)
    session_context = sys.modules["gateway.session_context"]

    monkeypatch.setattr(
        session_context, "get_session_env", lambda key, default: "research"
    )
    values = {"profile": "default", "runtime_id": "fallback-tools"}
    provider = plugin.Provider(
        SimpleNamespace(
            get_config=lambda key, default: values.get(key, default),
            profile_name="research",
        )
    )
    assert provider.settings()["runtime_id"] == "python-tools"
    assert provider.settings()["profile"] == "default"
    current["terminal"]["runtime_id"] = "shell-tools"
    assert provider.settings()["runtime_id"] == "shell-tools"
    current["terminal"]["runtime_id"] = ""
    assert provider.settings()["runtime_id"] == ""
    current["terminal"]["runtime_id"] = "../foreign"
    with pytest.raises(ValueError):
        provider.settings()


def test_personal_connection_requires_manager_host_identity(plugin, monkeypatch):
    monkeypatch.setenv("HERMES_PERSONAL_INSTANCE_ID", "fixture-instance")
    environment = plugin.ManagedSSHEnvironment.__new__(plugin.ManagedSSHEnvironment)
    environment.managed_generation = None
    with pytest.raises(RuntimeError, match="verified SSH host identity"):
        environment._connect(
            {
                "generation": "1",
                "environment": "00000000-0000-4000-8000-000000000001",
                "ssh": {"host": "10.42.0.1", "port": 2222, "user": "worker"},
            }
        )
