"""Budget all nested work against the actual owning Job and validate transport paths."""

import os
from datetime import UTC, datetime, timedelta
from types import SimpleNamespace
from unittest.mock import patch

import pytest
from platform_recovery import budget
from platform_recovery.port import Repository

pytestmark = pytest.mark.unit


@pytest.fixture(autouse=True)
def reset_budget():
    yield
    budget.configure(None)


def test_nested_commands_share_one_deadline():
    from platform_recovery.commands import run

    budget.configure(100)
    with (
        patch.object(budget.time, "monotonic", return_value=101),
        pytest.raises(RuntimeError, match="deadline"),
    ):
        run(["a-command-that-must-not-start"])


def test_hashing_observes_deadline_between_chunks(tmp_path):
    path = tmp_path / "state"
    path.write_bytes(b"data")
    budget.configure(100)
    with (
        patch.object(budget.time, "monotonic", return_value=101),
        pytest.raises(TimeoutError),
    ):
        budget.file_sha256(path)


def test_budget_includes_bootstrap_time_and_job_uid():
    owner = SimpleNamespace(
        name="bootstrap", uid="job-uid", kind="Job", controller=True
    )
    pod = SimpleNamespace(
        metadata=SimpleNamespace(uid="pod-uid", owner_references=[owner])
    )
    job = SimpleNamespace(
        metadata=SimpleNamespace(uid="job-uid"),
        spec=SimpleNamespace(active_deadline_seconds=1200),
        status=SimpleNamespace(start_time=datetime.now(UTC) - timedelta(seconds=400)),
    )
    with (
        patch.dict(os.environ, {"POD_NAME": "pod", "POD_UID": "pod-uid"}),
        patch.object(budget.kubernetes.config, "load_incluster_config"),
        patch.object(budget.kube, "CoreV1Api") as core,
        patch.object(budget.kube, "BatchV1Api") as batch,
        patch.object(budget.time, "monotonic", return_value=1000),
    ):
        core.return_value.read_namespaced_pod.return_value = pod
        batch.return_value.read_namespaced_job.return_value = job
        config = budget.for_job({"namespace": "agents"})
        assert 700 <= config["_operation_deadline"] - 1000 <= 710
        job.metadata.uid = "foreign-job"
        with pytest.raises(RuntimeError, match="matching"):
            budget.for_job({"namespace": "agents"})


def test_transport_cannot_return_a_checkpoint_outside_target(tmp_path):
    target = tmp_path / "target"
    target.mkdir()
    foreign = tmp_path / "foreign"
    foreign.mkdir()
    (target / "alias").symlink_to(foreign, target_is_directory=True)
    repository = Repository({})
    with (
        patch.object(
            repository, "request", return_value={"checkpoint": str(target / "alias")}
        ),
        patch("platform_recovery.contract.validate_payload") as validate,
        pytest.raises(ValueError, match="escapes"),
    ):
        repository.verify({}, target)
    validate.assert_not_called()


def test_transport_payload_validation_runs_once(tmp_path):
    target = tmp_path / "target"
    target.mkdir()
    payload = target / "payload"
    payload.mkdir()
    repository = Repository({})
    with (
        patch.object(
            repository,
            "request",
            return_value={"checkpoint": str(payload), "manifest": "untrusted"},
        ),
        patch(
            "platform_recovery.contract.validate_payload",
            return_value={"verified": True},
        ) as validate,
    ):
        assert repository.verify({}, target) == (payload, {"verified": True})
    validate.assert_called_once()
