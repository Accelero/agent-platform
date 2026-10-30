"""Native protocol retries and cache isolation for approved runtime selection."""

import importlib.util
import json
import sys
import threading
import types
import urllib.error
import uuid
from pathlib import Path
from types import SimpleNamespace

import jsonschema
import pytest
import yaml

pytestmark = pytest.mark.unit

SOURCE = (
    Path(__file__).resolve().parents[2]
    / "runtimes/hermes/plugins/managed_execution/__init__.py"
)


@pytest.fixture
def plugin(monkeypatch, tmp_path):
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
    # The manager writes this connection into every personal instance.
    managed = tmp_path / "config.yaml"
    connection = {
        "manager_url": "https://manager.example.org/execution",
        "token_file": "/run/hermes/credentials/execution-client-token",
        "ca_file": "/run/hermes/execution-ca/ca.crt",
        "ssh_key": "/run/hermes/credentials/id_ed25519",
    }
    managed.write_text(
        json.dumps(
            {"plugins": {"entries": {"managed_execution": {"settings": connection}}}}
        )
    )
    monkeypatch.setattr(module, "MANAGED_CONFIG", managed)
    return module


def settings(**changes):
    return {
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


def test_absent_selector_requests_the_default_runtime(plugin, monkeypatch):
    client = plugin.Client(settings())
    monkeypatch.setattr(client, "call", lambda path, body, timeout: body)
    assert "runtime_id" not in client.acquire("conversation")


def test_factory_cache_separates_runtime_and_session_close_targets_selection(
    plugin, monkeypatch
):
    config = {"runtime_id": "python-tools"}
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
                "session": "conversation",
                "runtime": environment.runtime,
                "runtime_id": "python-tools",
            },
        )
    ]


def test_dashboard_profile_selection_overrides_plugin_default(plugin, monkeypatch):
    config = sys.modules["hermes_cli.config"]

    current = {"terminal": {"runtime_id": "python-tools"}}
    monkeypatch.setattr(config, "load_config", lambda: current, raising=False)
    session_context = sys.modules["gateway.session_context"]

    monkeypatch.setattr(
        session_context, "get_session_env", lambda key, default: "research"
    )
    values = {"runtime_id": "fallback-tools"}
    provider = plugin.Provider(
        SimpleNamespace(
            get_config=lambda key, default: values.get(key, default),
            profile_name="research",
        )
    )
    assert provider.settings()["runtime_id"] == "python-tools"
    current["terminal"]["runtime_id"] = "shell-tools"
    assert provider.settings()["runtime_id"] == "shell-tools"
    current["terminal"]["runtime_id"] = ""
    assert provider.settings()["runtime_id"] == ""
    current["terminal"]["runtime_id"] = "../foreign"
    with pytest.raises(ValueError):
        provider.settings()


def test_personal_connection_requires_manager_host_identity(plugin):
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


CONTRACT = yaml.safe_load(
    (
        Path(__file__).resolve().parents[2]
        / "contracts/hermes-manager/execution.openapi.yaml"
    ).read_text()
)


def conforms(schema: str, path: str, body: dict) -> None:
    """Every plugin request must hit a contract path with a valid strict body."""
    assert "/execution" + path in CONTRACT["paths"]
    jsonschema.Draft202012Validator(
        {"$ref": f"#/components/schemas/{schema}", **CONTRACT}
    ).validate(body)


def recording_client(plugin, requests, reply=None):
    client = plugin.Client(
        settings(runtime_id="python-tools"),
        runtime={"hermes_home": "/opt/data", "workdir": "/workspace"},
    )
    client.call = lambda path, body, timeout: requests.append((path, body)) or reply
    return client


def test_acquire_request_matches_the_execution_contract(plugin):
    requests = []
    client = recording_client(plugin, requests, reply={"lease": "granted"})
    client.acquire("conversation", parent_lease=str(uuid.uuid4()))
    [(path, body)] = requests
    conforms("AcquireRequest", path, body)


def test_lease_and_session_requests_match_the_execution_contract(plugin):
    requests = []
    environment = plugin.ManagedSSHEnvironment.__new__(plugin.ManagedSSHEnvironment)
    environment._guard = threading.RLock()
    environment._closed = False
    environment._lost = False
    environment.settings = settings(runtime_id="python-tools")
    environment.session = "conversation"
    environment.runtime = {"hermes_home": "/opt/data", "workdir": "/workspace"}
    environment.client = recording_client(plugin, requests)
    environment.provider = SimpleNamespace(
        _guard=threading.RLock(), _environments=set()
    )
    lease = str(uuid.uuid4())
    environment._reply = {"lease": lease, "ttl": 120}
    environment._renew_stop = threading.Event()
    environment._renew_worker = threading.Thread(target=lambda: None)
    environment._renew_worker.start()
    # One renewal cycle, then stop.
    stops = iter([False, True])
    environment._renew(
        environment._reply, SimpleNamespace(wait=lambda _seconds: next(stops))
    )
    environment.cleanup()
    schemas = {
        "/v1/leases/heartbeat": "LeaseRequest",
        "/v1/leases/release": "LeaseRequest",
        "/v1/sessions/close": "CloseSessionRequest",
    }
    assert [path for path, _ in requests] == list(schemas)
    for path, body in requests:
        conforms(schemas[path], path, body)


def test_lifetime_mode_comes_only_from_the_managed_configuration(plugin):
    """Session close retires per-session workers; a profile cannot switch a
    personal instance to profile-lifetime workers that only retire when idle."""
    managed = json.loads(plugin.MANAGED_CONFIG.read_text())
    settings = managed["plugins"]["entries"]["managed_execution"]["settings"]
    profile = {"mode": "per_profile_always_running", "acquire_timeout_seconds": 1}
    provider = plugin.Provider(
        SimpleNamespace(
            get_config=lambda key, default: profile.get(key, default),
            profile_name="research",
        )
    )
    assert provider.settings()["mode"] == "per_session"
    assert provider.settings()["acquire_timeout_seconds"] == 180
    settings.update(mode="per_profile_on_demand", acquire_timeout_seconds=30)
    plugin.MANAGED_CONFIG.write_text(json.dumps(managed))
    assert provider.settings()["mode"] == "per_profile_on_demand"
    assert provider.settings()["acquire_timeout_seconds"] == 30


def test_workers_start_in_the_workspace_unless_a_directory_is_declared(
    plugin, monkeypatch
):
    """Hermes gives plugin backends root's home when nothing is declared, as in
    a scheduled run; the non-root worker starts in the workspace instead."""
    declared = {}
    for name, values in {
        "agent.runtime_cwd": {"scoped_session_cwd": lambda: None},
        "tools.terminal_tool": {"get_session_cwd": lambda task: declared.get(task)},
    }.items():
        module = types.ModuleType(name)
        module.__dict__.update(values)
        monkeypatch.setitem(sys.modules, name, module)
    assert plugin.invocation_cwd("/root", "cron") == "/workspace"
    assert plugin.invocation_cwd("", "cron") == "/workspace"
    assert plugin.invocation_cwd("/srv/project", "chat") == "/srv/project"
    declared["chat"] = "/workspace/project"
    assert plugin.invocation_cwd("/root", "chat") == "/workspace/project"
    # Every command carries the generic default too, not only the first.
    assert plugin.command_cwd("/root", "/workspace") == "/workspace"
    assert plugin.command_cwd("/workspace/src", "/workspace") == "/workspace/src"
