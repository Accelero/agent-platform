"""Invocation scopes mount only what they select and never follow symbolic links
(R1, R8).
"""

import pytest
from runtime_manager.scope import expand

from tests.support.world import DEFAULT_IMAGE

CLAIMS = {"claim": "hermes-tasks"}


def codex_template(root):
    return {
        "image": DEFAULT_IMAGE,
        "runtime_guest_root": str(root),
        "runtime_kubernetes_mounts": [
            {"path": ".multica/workspaces", "claim": "tasks"},
            {"path": "environments", "claim": "homes"},
            {"path": ".codex/multica-sessions", "claim": "history"},
        ],
        "kubernetes": {"role": "codex", "volumes": [], "volume_mounts": []},
    }


@pytest.fixture
def guest(tmp_path):
    """A guest root with a task, a worker home and a session store."""
    task = tmp_path / ".multica/workspaces/ws/task1"
    home = tmp_path / "environments/session1/home"
    store = tmp_path / ".codex/multica-sessions/ws/agent1/run1"
    for path in (task, home, store):
        path.mkdir(parents=True)
    return tmp_path, task, store


def scope(task, store=None, agent="agent1"):
    value = {"agent": agent, "task_root": str(task)}
    if store:
        value["sessions"] = str(store)
    return value


def test_a_codex_run_mounts_its_task_home_and_session_store_only(guest):
    root, task, store = guest
    result = expand(
        codex_template(root),
        runtime=scope(task, store),
        mode="per_session",
        profile="p",
        session="session1",
    )
    mounts = {
        m["mountPath"]: (m["name"], m["subPath"])
        for m in result["kubernetes"]["volume_mounts"]
    }
    assert mounts == {
        str(task): ("codex-runtime-0", "ws/task1"),
        "/home/worker": ("codex-runtime-1", "session1/home"),
        str(store): ("codex-runtime-2", "ws/agent1/run1"),
    }
    assert {
        v["persistentVolumeClaim"]["claimName"] for v in result["kubernetes"]["volumes"]
    } == {"tasks", "homes", "history"}


@pytest.mark.parametrize(
    ("mutate", "message"),
    [
        (lambda g: scope(g[1], g[2], agent="other"), "another agent"),
        (lambda g: {**scope(g[1]), "extra": "1"}, "Invalid Codex runtime scope"),
        (lambda g: scope(g[0] / ".multica/workspaces/ws"), "single task workspace"),
        (lambda g: scope(g[0] / ".multica/workspaces/ws/missing"), "missing"),
    ],
)
def test_a_codex_scope_must_name_one_task_and_its_own_agent(guest, mutate, message):
    runtime = mutate(guest)
    with pytest.raises(ValueError, match=message):
        expand(
            codex_template(guest[0]),
            runtime=runtime,
            mode="per_session",
            profile="p",
            session="session1",
        )


def test_codex_runs_are_session_scoped_and_never_follow_symbolic_links(
    guest, tmp_path_factory
):
    root, task, store = guest
    with pytest.raises(ValueError, match="Invalid Codex execution scope"):
        expand(
            codex_template(root),
            runtime=scope(task),
            mode="per_profile_on_demand",
            profile="p",
            session="s",
        )
    elsewhere = tmp_path_factory.mktemp("elsewhere")
    task.rmdir()
    task.symlink_to(elsewhere)
    with pytest.raises(ValueError, match="symlink"):
        expand(
            codex_template(root),
            runtime=scope(task),
            mode="per_session",
            profile="p",
            session="session1",
        )


@pytest.mark.parametrize(
    "runtime",
    [
        {"namespace": "other-namespace"},
        {"pvc": "private-data"},
        {"workdir": "/workspace", "pvc": "private-data"},
        {"workdir": "/workspace", "hermes_home": "/opt/data/../etc"},
        "/workspace",
    ],
)
def test_a_client_cannot_introduce_namespaces_claims_or_mount_specifications(runtime):
    """R1."""
    template = {
        "image": DEFAULT_IMAGE,
        "workdir_scope": {"controller_root": "/opt/data/.multica/workspaces"},
        "kubernetes": {"role": "hermes", "volumes": [], "volume_mounts": []},
    }
    with pytest.raises(ValueError):
        expand(template, runtime=runtime, mode="per_session", profile="p", session="s")
    plain = {"image": DEFAULT_IMAGE, "kubernetes": {"role": "hermes"}}
    with pytest.raises(ValueError, match="not permitted"):
        expand(
            plain,
            runtime={"workdir": "/workspace"},
            mode="per_session",
            profile="p",
            session="s",
        )
    assert (
        expand(plain, runtime={}, mode="per_session", profile="p", session="s") is plain
    )
