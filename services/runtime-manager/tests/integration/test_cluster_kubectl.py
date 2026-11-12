"""The cluster adapter's commands, run through a real `kubectl` executable (a fake
one).
"""

import os
import stat
import sys
from pathlib import Path

import pytest
from runtime_manager.cluster import KubernetesCluster
from runtime_manager.domain.model import PodState, pod_name

from agent_platform.build import ROOT
from tests.support.world import DEFAULT_IMAGE

ENVIRONMENT = "a" * 24
NAME = pod_name(ENVIRONMENT, "b" * 12)
TEMPLATE = {"image": DEFAULT_IMAGE, "kubernetes": {"role": "hermes"}}


@pytest.fixture
def cluster(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> KubernetesCluster:
    binaries = tmp_path / "bin"
    binaries.mkdir()
    shim = binaries / "kubectl"
    shim.write_text(
        f'#!/bin/sh\nexec "{sys.executable}" '
        f'"{ROOT}/tests/support/fake_kubectl.py" "$@"\n'
    )
    shim.chmod(shim.stat().st_mode | stat.S_IXUSR)
    monkeypatch.setenv("PATH", f"{binaries}{os.pathsep}{os.environ['PATH']}")
    monkeypatch.setenv("FAKE_KUBE_STATE", str(tmp_path / "cluster.json"))
    monkeypatch.delenv("KUBERNETES_SERVICE_HOST", raising=False)
    return KubernetesCluster(
        {"namespace": "hermes-execution", "kubeconfig": "/dev/null"}
    )


def test_a_pod_is_created_observed_listed_and_deleted_by_name(cluster):
    assert cluster.observe(NAME) is None
    cluster.create(NAME, ENVIRONMENT, TEMPLATE)
    cluster.create(NAME, ENVIRONMENT, TEMPLATE)  # creating twice is harmless
    pod = cluster.observe(NAME)
    assert pod.state is PodState.READY and pod.endpoint["port"] == 2222
    assert [listed.name for listed in cluster.list_managed()] == [NAME]
    cluster.delete(NAME)
    assert cluster.observe(NAME) is None
    cluster.delete(NAME)  # so is deleting twice


def test_a_pod_of_another_environment_or_owner_is_never_touched(cluster):
    cluster.create(NAME, ENVIRONMENT, TEMPLATE)
    with pytest.raises(ValueError, match="does not match"):
        cluster.create(NAME, "c" * 24, TEMPLATE)
    with pytest.raises(ValueError, match="does not match"):
        cluster.create(NAME, ENVIRONMENT, {**TEMPLATE, "kubernetes": {"role": "other"}})
    with pytest.raises(ValueError, match="Invalid managed Pod name"):
        cluster.observe("someone-elses-pod")
    with pytest.raises(ValueError, match="Invalid managed Pod name"):
        cluster.delete("hermes-exec-x")


def test_a_failing_command_raises_a_bounded_message(cluster, monkeypatch):
    monkeypatch.delenv("FAKE_KUBE_STATE")
    with pytest.raises(RuntimeError) as error:
        cluster.observe(NAME)
    assert str(error.value).startswith("Kubernetes execution operation failed")
    assert len(str(error.value)) < 400
