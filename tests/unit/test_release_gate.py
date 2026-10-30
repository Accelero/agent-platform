"""The release gate: a tag publishes only a commit whose workflows passed."""

from pathlib import Path

import pytest
import yaml

from agent_platform.release_gate import REQUIRED, problems, runs_of

pytestmark = pytest.mark.unit

ROOT = Path(__file__).resolve().parents[2]
SHA = "a" * 40
OTHER = "b" * 40


def run(
    name,
    conclusion="success",
    status="completed",
    sha=SHA,
    created="2026-10-30T10:00:00Z",
):
    return {
        "name": name,
        "head_sha": sha,
        "status": status,
        "conclusion": conclusion,
        "created_at": created,
    }


def passing():
    return [run(name) for name in REQUIRED]


def test_a_commit_whose_workflows_all_passed_is_released():
    assert problems(passing(), SHA) == []


@pytest.mark.parametrize("missing", REQUIRED)
def test_a_commit_missing_a_required_workflow_is_refused(missing):
    runs = [item for item in passing() if item["name"] != missing]
    assert problems(runs, SHA) == [f"{missing}: no run for this commit"]


@pytest.mark.parametrize(
    "conclusion", ["failure", "cancelled", "skipped", "timed_out", "neutral", None]
)
def test_a_workflow_that_did_not_succeed_refuses_the_release(conclusion):
    runs = [run("Checks"), run("Cluster scenarios", conclusion=conclusion)]
    assert problems(runs, SHA) == [f"Cluster scenarios: the latest run {conclusion}"]


@pytest.mark.parametrize("status", ["queued", "in_progress", "waiting"])
def test_a_workflow_still_running_refuses_the_release(status):
    runs = [run("Checks", status=status, conclusion=None), run("Cluster scenarios")]
    assert problems(runs, SHA) == [f"Checks: the latest run is {status}"]


def test_only_runs_for_the_exact_commit_count():
    runs = [run(name, sha=OTHER) for name in REQUIRED]
    assert len(problems(runs, SHA)) == len(REQUIRED)
    assert problems(runs + passing(), SHA) == []


def test_the_latest_run_decides_in_either_direction():
    later_failure = [
        run("Checks", created="2026-10-30T09:00:00Z"),
        run("Checks", conclusion="failure", created="2026-10-30T11:00:00Z"),
        run("Cluster scenarios"),
    ]
    assert problems(later_failure, SHA) == ["Checks: the latest run failure"]
    later_success = [
        run("Checks", conclusion="failure", created="2026-10-30T09:00:00Z"),
        run("Checks", created="2026-10-30T11:00:00Z"),
        run("Cluster scenarios"),
    ]
    assert problems(later_success, SHA) == []


def test_runs_of_other_workflows_do_not_stand_in():
    runs = [run("Release agent platform"), run("Build cache"), run("Checks")]
    assert problems(runs, SHA) == ["Cluster scenarios: no run for this commit"]


def test_pages_from_the_api_are_read_whole():
    page = {"workflow_runs": [run("Checks")]}
    other = {"workflow_runs": [run("Cluster scenarios")]}
    assert runs_of(page) == page["workflow_runs"]
    assert problems(runs_of([page, other]), SHA) == []
    assert runs_of({}) == [] and runs_of([{}, {}]) == []


def test_the_required_names_are_the_workflows_in_the_repository():
    names = {
        yaml.safe_load(path.read_text())["name"]
        for path in (ROOT / ".github/workflows").glob("*.yml")
    }
    assert set(REQUIRED) <= names


def test_publication_waits_for_the_gate_which_reads_the_runs_of_the_tagged_commit():
    workflow = yaml.safe_load((ROOT / ".github/workflows/release.yml").read_text())
    assert workflow["jobs"]["publish"]["needs"] == "gate"
    assert workflow["permissions"]["actions"] == "read"
    steps = "\n".join(step.get("run", "") for step in workflow["jobs"]["gate"]["steps"])
    assert "head_sha=$GITHUB_SHA" in steps
    assert "agent_platform.release_gate" in steps and '--sha "$GITHUB_SHA"' in steps
    # Nothing that builds or publishes is in the gate job.
    assert "agent_platform.release " not in steps and "--publish" not in steps
