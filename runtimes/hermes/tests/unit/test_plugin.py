"""Client lease loss, endpoint validation and host identity (X14, X15)."""

import ast
from types import SimpleNamespace

import pytest
import yaml

from tests.support.plugin import ROOT, SOURCE, Ticks, load_plugin


@pytest.mark.parametrize("status", [403, 409, 503])
def test_heartbeat_refusals_end_the_lease_at_the_right_time(monkeypatch, status):
    plugin = load_plugin(monkeypatch)
    now = [0]
    monkeypatch.setattr(plugin, "time", SimpleNamespace(monotonic=lambda: now[0]))
    attempts = []

    def call(*args, **kwargs):
        attempts.append(now[0])
        raise plugin.ManagerRefusal(status, "lease_expired", "lease has ended")

    env = object.__new__(plugin.ManagedSSHEnvironment)
    env.client, env._lost = SimpleNamespace(call=call), False
    stop = Ticks(lambda seconds: now.__setitem__(0, now[0] + seconds))
    env._renew({"ttl": 120, "lease": "fixture"}, stop)
    assert env._lost
    assert attempts == ([15] if status in (403, 409) else [15, 30, 45, 60])


def test_a_provisioned_host_key_replaces_trust_on_first_use(monkeypatch, tmp_path):
    plugin = load_plugin(monkeypatch)
    key = tmp_path / "host-key.pub"
    # The public encoding is validated by OpenSSH at connection time.
    key.write_text("ssh-ed25519 " + "A" * 68 + " fixture\n")
    env = object.__new__(plugin.ManagedSSHEnvironment)
    env.settings = {"host_key_file": str(key)}
    env.managed_environment, env.managed_generation = "a" * 24, "b" * 12
    env._pin_host_key()
    try:
        flags = env._target_flags("-p")
        assert "StrictHostKeyChecking=yes" in flags
        assert "GlobalKnownHostsFile=/dev/null" in flags
        assert "UserKnownHostsFile=" + str(env._known_hosts) in flags
        assert "HostKeyAlias=" + env._host_alias() in flags
        assert env._known_hosts.read_text() == (
            env._host_alias() + " ssh-ed25519 " + "A" * 68 + "\n"
        )
        assert env._known_hosts.stat().st_mode & 0o777 == 0o600
        assert "StrictHostKeyChecking=accept-new" not in env._build_ssh_command()
        assert "StrictHostKeyChecking=yes" in env._build_ssh_command()
        key.unlink()
        env._pin_host_key()
        assert env._known_hosts is None
        assert "StrictHostKeyChecking=accept-new" in env._build_ssh_command()
        assert env._target_flags("-p") == ["-o", "HostKeyAlias=" + env._host_alias()]
        key.write_text("not a host key")
        with pytest.raises(RuntimeError, match="Invalid worker SSH host key"):
            env._pin_host_key()
    finally:
        if env._host_identity_dir:
            env._host_identity_dir.cleanup()


@pytest.mark.parametrize(
    "endpoint",
    [
        {"host": "10.0.0.1", "port": 22, "user": "worker"},
        {"host": "10.0.0.1", "port": 2222, "user": "root"},
        {"host": "example.org", "port": 2222, "user": "worker"},
        {"host": "-oProxyCommand=bad", "port": 2222, "user": "worker"},
    ],
)
def test_ssh_endpoints_are_validated_before_connecting(monkeypatch, endpoint):
    plugin = load_plugin(monkeypatch)
    env = object.__new__(plugin.ManagedSSHEnvironment)
    env.managed_generation = None
    with pytest.raises((ValueError, RuntimeError)):
        env._connect({"generation": "b" * 12, "environment": "a" * 24, "ssh": endpoint})


def test_plugin_defaults_to_profile_scoped_on_demand_workers():
    schema = yaml.safe_load(
        (ROOT / "runtimes/hermes/plugins/managed_execution/plugin.yaml").read_text()
    )["config_schema"]
    calls = [
        node
        for node in ast.walk(ast.parse(SOURCE.read_text()))
        if isinstance(node, ast.Call)
        and isinstance(node.func, ast.Name)
        and node.func.id == "dict"
    ]
    modes = [
        ast.literal_eval(kw.value)
        for call in calls
        for kw in call.keywords
        if kw.arg == "mode"
    ]
    assert modes == [schema["mode"]["default"]] == ["per_profile_on_demand"]
    assert "settings['mode'] = 'per_session'" in SOURCE.read_text()
