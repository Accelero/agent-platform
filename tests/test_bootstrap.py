"""Exercise fresh enrollment, live-state preservation and interrupted checkpoints."""

import importlib.util
import json
from contextlib import nullcontext
from copy import deepcopy
from pathlib import Path
from unittest.mock import Mock, patch

import pytest

from agent_platform.build import ROOT
from tests.test_chart import render

SOURCE = ROOT / "services/tools/platform_bootstrap/__main__.py"
spec = importlib.util.spec_from_file_location("bootstrap", SOURCE)
bootstrap = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bootstrap)


@pytest.fixture
def scenario(tmp_path):
    config = {
        "installation": "fixture",
        "namespace": "agents",
        "executionNamespace": "execution",
        "onboardingSecret": "agent-onboarding",
        "profileSecret": "hermes-task-login",
        "profile": "researcher",
        "email": "owner@example.org",
        "workspace": {"name": "Research", "slug": "research", "issuePrefix": "RES"},
        "home": str(tmp_path),
        "serverUrl": "http://multica-backend:8080",
        "publicUrl": "https://board.example.org",
        "agent": {
            "name": "Assistant",
            "permission_mode": "private",
            "max_concurrent_tasks": 1,
        },
    }
    stored = {}
    cluster = Mock()
    cluster.lock.side_effect = nullcontext
    cluster.recovery.return_value = True
    cluster.replicas.return_value = {"hermes": 1, "multica-backend": 1}
    cluster.secret.side_effect = lambda name: deepcopy(stored.get(name, {}))
    cluster.save.side_effect = lambda name, values, *args: stored.setdefault(
        name, {}
    ).update(values)

    def native(config, action, **values):
        return {
            "inspect": {"state": "empty"},
            "generate": {"daemon": "daemon-fixture", "profile": "profile-fixture"},
            "ensure": {"user": "owner-id", "workspace": "workspace-id"},
            "complete": {},
        }[action]

    board = Mock()
    board.request.side_effect = lambda path, *args: {
        "/api/runtimes": [
            {"id": "runtime-id", "provider": "hermes", "status": "online"}
        ],
        "/api/agents": [],
    }[path]
    with (
        patch.object(bootstrap, "native", side_effect=native) as command,
        patch.object(bootstrap, "Board", return_value=board),
        patch.object(bootstrap, "hook") as hook,
    ):
        yield config, cluster, stored, command, board, hook


def test_first_start_creates_both_native_configs_and_only_one_assistant(scenario):
    config, cluster, stored, command, board, hook = scenario
    assert bootstrap.enroll(config, cluster, force=False).startswith("enrolled")
    assert [call.args[1] for call in command.call_args_list] == [
        "inspect",
        "generate",
        "ensure",
        "complete",
    ]
    assert [call.args[1] for call in hook.call_args_list] == [
        "before",
        "after",
        "checkpoint",
    ]
    daemon = json.loads((Path(config["home"]) / ".multica/config.json").read_text())
    profile = json.loads(
        (Path(config["home"]) / "profiles/researcher/.multica/config.json").read_text()
    )
    assert daemon["token"] != profile["token"]
    assert daemon["workspace_id"] == profile["workspace_id"] == "workspace-id"
    assert json.loads(stored["agent-onboarding"]["bootstrap.json"])["pending"] is False
    assert json.loads(stored["hermes-task-login"]["config.json"]) == profile
    posts = [call for call in board.request.call_args_list if len(call.args) == 2]
    assert len(posts) == 1
    assert posts[0].args[1]["custom_args"] == ["-p", "researcher"]


@pytest.mark.parametrize("journal", ["{}", "malformed"])
def test_initialized_database_ignores_credential_drift(scenario, journal):
    config, cluster, stored, command, board, hook = scenario
    stored["agent-onboarding"] = {
        "daemon.json": "modified live credentials",
        "bootstrap.json": journal,
    }
    command.side_effect = lambda *args, **kwargs: {"state": "initialized"}
    assert "unchanged" in bootstrap.enroll(config, cluster, force=False)
    cluster.save.assert_not_called()
    hook.assert_not_called()
    board.request.assert_not_called()


def test_empty_restored_database_never_triggers_fresh_bootstrap(scenario):
    config, cluster, _, command, _, hook = scenario
    cluster.recovery.return_value = False
    with pytest.raises(RuntimeError, match="restored"):
        bootstrap.enroll(config, cluster, force=False)
    assert command.call_count == 1
    hook.assert_not_called()


def test_recovery_failure_never_mints_credentials(scenario):
    config, cluster, _, command, _, _ = scenario
    cluster.recovery.side_effect = RuntimeError("recovery pending")
    with pytest.raises(RuntimeError, match="recovery"):
        bootstrap.enroll(config, cluster, force=False)
    command.assert_not_called()


def test_retry_reuses_pending_credentials_and_does_not_duplicate_agent(scenario):
    config, cluster, stored, command, board, hook = scenario
    stored["agent-onboarding"] = {
        "bootstrap.json": json.dumps(
            {
                "pending": True,
                "force": False,
                "daemon": "original-daemon",
                "profile": "original-profile",
                "previous": "",
                "workspace": "",
                "replicas": {"hermes": 1},
            }
        )
    }
    original = command.side_effect
    command.side_effect = lambda config, action, **kwargs: (
        {"state": "pending"}
        if action == "inspect"
        else original(config, action, **kwargs)
    )
    board.request.side_effect = lambda path, *args: (
        [{"id": "runtime-id", "provider": "hermes", "status": "online"}]
        if path == "/api/runtimes"
        else [
            {
                "id": "agent-id",
                "runtime_id": "runtime-id",
                "name": "User renamed assistant",
            }
        ]
    )
    bootstrap.enroll(config, cluster, force=False)
    assert "generate" not in [call.args[1] for call in command.call_args_list]
    ensure = next(call for call in command.call_args_list if call.args[1] == "ensure")
    assert ensure.kwargs["daemon"] == "original-daemon"
    assert all(len(call.args) == 1 for call in board.request.call_args_list)


def test_force_updates_auth_without_overwriting_native_preferences(scenario):
    config, cluster, stored, command, _, _ = scenario
    command.side_effect = lambda config, action, **kwargs: {
        "inspect": {"state": "initialized"},
        "generate": {"daemon": "new-daemon", "profile": "new-profile"},
        "ensure": {"user": "same-user", "workspace": "same-workspace"},
        "complete": {},
    }[action]
    path = Path(config["home"]) / ".multica/config.json"
    path.parent.mkdir(parents=True)
    path.with_name("daemon.id").write_text("00000000-0000-0000-0000-000000000001")
    path.write_text(
        json.dumps(
            {
                "token": "old",
                "workspace_id": "same-workspace",
                "server_url": config["serverUrl"],
                "custom_setting": "keep",
            }
        )
    )
    stored["agent-onboarding"] = {"daemon.json": path.read_text()}
    bootstrap.enroll(config, cluster, force=True)
    assert json.loads(path.read_text())["custom_setting"] == "keep"
    ensure = next(call for call in command.call_args_list if call.args[1] == "ensure")
    assert ensure.kwargs["previous"] == "old"
    assert ensure.kwargs["workspace"] == "same-workspace"


def test_failed_checkpoint_retries_only_checkpoint_without_overwriting_live_auth(
    scenario,
):
    config, cluster, stored, command, _, hook = scenario
    hook.side_effect = lambda config, phase: (
        (_ for _ in ()).throw(RuntimeError("failed checkpoint"))
        if phase == "checkpoint"
        else None
    )
    with pytest.raises(RuntimeError, match="checkpoint"):
        bootstrap.enroll(config, cluster, force=False)
    journal = json.loads(stored["agent-onboarding"]["bootstrap.json"])
    assert journal["pending"] and journal["mutation_complete"]
    hook.reset_mock(side_effect=True)
    command.reset_mock()
    command.side_effect = lambda *args, **kwargs: {"state": "initialized"}
    cluster.recovery.return_value = False
    assert "checkpoint verified" in bootstrap.enroll(config, cluster, force=False)
    assert command.call_count == 1
    assert [call.args[1] for call in hook.call_args_list] == ["checkpoint"]


def test_chart_ships_automatic_job_and_suspended_manual_template(tmp_path):
    values = {
        "multica": {"ownerEmail": "owner@example.org"},
        "bootstrap": {
            "enabled": True,
            "installation": "fixture",
            "lifecycle": {
                key: ["fixture", key] for key in ("before", "after", "checkpoint")
            },
        },
        "images": {"tools": "fixture-tools"},
    }
    objects = render(tmp_path, values)
    job = objects["Job", "agents-bootstrap-1"]
    assert job["spec"]["activeDeadlineSeconds"] == 1200
    cron = objects["CronJob", "agents-bootstrap"]
    assert cron["spec"]["suspend"] is True
    pod = job["spec"]["template"]["spec"]
    assert pod["containers"][0]["command"] == ["python", "-m", "platform_bootstrap"]
    assert pod["containers"][0]["args"] == []
    assert pod["securityContext"]["runAsUser"] == 1000
    assert not any(kind == "Secret" for kind, _ in objects)
    configuration = json.loads(
        objects["ConfigMap", "agents-bootstrap"]["data"]["config.json"]
    )
    assert configuration["createAgent"] is True
    assert configuration["runtimeVisibility"] == "private"
    values["bootstrap"]["createAgent"] = False
    runtime_only = render(tmp_path, values)
    configuration = json.loads(
        runtime_only["ConfigMap", "agents-bootstrap"]["data"]["config.json"]
    )
    assert configuration["createAgent"] is False


def test_bootstrap_waits_for_every_pinned_native_migration():
    manifest = ROOT / "services/multica/bootstrap/migrations.txt"
    expected = sorted(
        path.name.removesuffix(".up.sql")
        for path in (ROOT / "vendor/multica/server/migrations").glob("*.up.sql")
    )
    assert manifest.read_text().splitlines() == expected


def test_force_refuses_missing_native_daemon_identity_before_mutation(scenario):
    config, cluster, stored, command, board, hook = scenario
    original = command.side_effect
    command.side_effect = lambda config, action, **kwargs: (
        {"state": "initialized"}
        if action == "inspect"
        else original(config, action, **kwargs)
    )
    with pytest.raises(RuntimeError, match="original native daemon identity"):
        bootstrap.enroll(config, cluster, force=True)
    hook.assert_not_called()
    assert "ensure" not in [call.args[1] for call in command.call_args_list]


def test_secret_repair_refuses_foreign_installation_labels():
    config = {"namespace": "agents", "labels": {"installation": "ours"}}
    with patch.object(bootstrap.kube, "CoreV1Api") as core:
        core.return_value.read_namespaced_secret.return_value.metadata.labels = {
            "installation": "foreign"
        }
        cluster = bootstrap.Cluster(config)
        with pytest.raises(RuntimeError, match="another installation"):
            cluster.save("agent-onboarding", {"daemon.json": "private fixture"})
        core.return_value.patch_namespaced_secret.assert_not_called()


def test_default_enrollment_uses_root_hermes_without_creating_a_profile(scenario):
    config, cluster, stored, _, board, _ = scenario
    config["profile"] = ""
    bootstrap.enroll(config, cluster, force=False)
    home = Path(config["home"])
    assert not (home / "profiles").exists()
    daemon = json.loads((home / ".multica/config.json").read_text())
    profile = json.loads(stored["hermes-task-login"]["config.json"])
    assert daemon["token"] == "daemon-fixture"
    assert profile["token"] == "profile-fixture"
    posts = [call for call in board.request.call_args_list if len(call.args) == 2]
    assert posts[0].args[1]["custom_args"] == []


def test_runtime_only_bootstrap_registers_hermes_without_creating_an_agent(scenario):
    config, cluster, stored, command, board, hook = scenario
    config["createAgent"] = False
    config["runtimeVisibility"] = "public"
    config["profile"] = ""
    responses = {
        "/api/runtimes": [
            {
                "id": "runtime-id",
                "provider": "hermes",
                "status": "online",
                "visibility": "private",
            }
        ],
        "/api/agents": [],
        "/api/runtimes/runtime-id": {},
    }
    board.request.side_effect = lambda path, *args, **kwargs: responses[path]
    assert bootstrap.enroll(config, cluster, force=False).startswith("enrolled")
    writes = [call for call in board.request.call_args_list if len(call.args) == 2]
    assert [(call.args, call.kwargs) for call in writes] == [
        (("/api/runtimes/runtime-id", {"visibility": "public"}), {"method": "PATCH"})
    ]
    assert (Path(config["home"]) / ".multica/config.json").is_file()
    assert not (Path(config["home"]) / "profiles").exists()
    assert json.loads(stored["agent-onboarding"]["bootstrap.json"])["pending"] is False


def test_cluster_wait_retries_until_the_pod_network_is_ready():
    import urllib3

    cluster = Mock()
    cluster.config = {"recoveryGate": "agent-platform-recovery"}
    cluster.namespace = "agents"
    cluster.core.read_namespaced_config_map.side_effect = [
        urllib3.exceptions.MaxRetryError(None, "/", None),
        OSError("connection refused"),
        {},
    ]
    with patch.object(bootstrap.time, "sleep") as sleep:
        bootstrap.wait_for_cluster(cluster)
    assert cluster.core.read_namespaced_config_map.call_count == 3
    assert sleep.call_count == 2


def test_cluster_wait_does_not_retry_authorization_errors():
    import kubernetes.client as kube

    cluster = Mock()
    cluster.config = {"recoveryGate": "agent-platform-recovery"}
    cluster.namespace = "agents"
    cluster.core.read_namespaced_config_map.side_effect = kube.ApiException(status=403)
    with patch.object(bootstrap.time, "sleep") as sleep:
        with pytest.raises(kube.ApiException):
            bootstrap.wait_for_cluster(cluster)
    sleep.assert_not_called()


def test_cluster_wait_has_a_deadline():
    cluster = Mock()
    cluster.config = {"recoveryGate": "agent-platform-recovery"}
    cluster.namespace = "agents"
    cluster.core.read_namespaced_config_map.side_effect = OSError("refused")
    with (
        patch.object(bootstrap.time, "monotonic", side_effect=[0, 1, 200]),
        patch.object(bootstrap.time, "sleep"),
        pytest.raises(TimeoutError, match="reachable"),
    ):
        bootstrap.wait_for_cluster(cluster)
