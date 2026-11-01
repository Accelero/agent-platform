"""Pods as the cluster shows and receives them (X3, R9)."""

import pytest
from runtime_manager.cluster import parse_pod, render_pod
from runtime_manager.domain.model import POD_NAME, Failure, PodState

from tests.support.world import DEFAULT_IMAGE

pytestmark = pytest.mark.unit

NAME = "hermes-exec-" + "a" * 24 + "-" + "b" * 12
READY = [{"type": "Ready", "status": "True"}]


def raw(status=None, **metadata):
    return {"metadata": {"name": NAME, **metadata}, "status": status or {}}


def waiting(reason):
    return {"containerStatuses": [{"state": {"waiting": {"reason": reason}}}]}


@pytest.mark.parametrize(
    ("status", "metadata", "state", "failure"),
    [
        (
            {"phase": "Running", "podIP": "10.0.0.5", "conditions": READY},
            {},
            PodState.READY,
            None,
        ),
        (
            {"phase": "Pending", **waiting("ContainerCreating")},
            {},
            PodState.PENDING,
            None,
        ),
        (
            {
                "phase": "Pending",
                "conditions": [{"type": "PodScheduled", "status": "False"}],
            },
            {},
            PodState.PENDING,
            None,
        ),
        ({"phase": "Running", "conditions": READY}, {}, PodState.PENDING, None),
        (
            {"phase": "Pending", **waiting("ImagePullBackOff")},
            {},
            PodState.PENDING,
            Failure.IMAGE_UNAVAILABLE,
        ),
        (
            {"phase": "Pending", **waiting("ErrImagePull")},
            {},
            PodState.PENDING,
            Failure.IMAGE_UNAVAILABLE,
        ),
        (
            {"phase": "Pending", **waiting("InvalidImageName")},
            {},
            PodState.PENDING,
            Failure.IMAGE_UNAVAILABLE,
        ),
        (
            {"phase": "Pending", **waiting("CreateContainerConfigError")},
            {},
            PodState.PENDING,
            Failure.START_FAILED,
        ),
        (
            {"phase": "Running", **waiting("CrashLoopBackOff")},
            {},
            PodState.PENDING,
            Failure.START_FAILED,
        ),
        ({"phase": "Failed"}, {}, PodState.FAILED, Failure.START_FAILED),
        ({"phase": "Succeeded"}, {}, PodState.FAILED, Failure.START_FAILED),
        (
            {"phase": "Failed", **waiting("ImagePullBackOff")},
            {},
            PodState.FAILED,
            Failure.IMAGE_UNAVAILABLE,
        ),
        (
            {"phase": "Running", "podIP": "10.0.0.5", "conditions": READY},
            {"deletionTimestamp": "2026-01-01T00:00:00Z"},
            PodState.TERMINATING,
            None,
        ),
    ],
)
def test_pods_are_classified_by_what_waiting_can_still_fix(
    status, metadata, state, failure
):
    """X3: an unschedulable or merely slow Pod is neither an image nor a start
    failure.
    """
    pod = parse_pod(raw(status, **metadata))
    assert (pod.state, pod.failure) == (state, failure)


def test_a_ready_pod_carries_the_ssh_endpoint_and_creation_time():
    pod = parse_pod(
        raw(
            {"phase": "Running", "podIP": "10.0.0.5", "conditions": READY},
            creationTimestamp="2026-11-01T12:00:00Z",
        )
    )
    assert pod.endpoint == {"host": "10.0.0.5", "port": 2222, "user": "worker"}
    assert pod.created == 1793534400.0


def test_a_completed_init_container_is_not_a_failure():
    status = {
        "phase": "Running",
        "podIP": "10.0.0.5",
        "conditions": READY,
        "initContainerStatuses": [{"state": {"terminated": {"exitCode": 0}}}],
    }
    assert parse_pod(raw(status)).state is PodState.READY


def render(**spec):
    template = {"image": DEFAULT_IMAGE, "kubernetes": {"role": "hermes", **spec}}
    return render_pod(
        NAME, "a" * 24, template, "hermes-execution", [{"name": "registry"}]
    )


def test_worker_pods_run_non_root_without_tokens_privileges_or_overrides():
    """R9: the shape the chart's admission policy admits."""
    pod = render(uid=2000, gid=3000)
    spec, container = pod["spec"], pod["spec"]["containers"][0]
    assert POD_NAME.fullmatch(pod["metadata"]["name"])
    assert pod["metadata"]["labels"] == {
        "app.kubernetes.io/managed-by": "agent-platform-runtime-manager",
        "hermes-execution": "a" * 24,
        "hermes.stack/role": "hermes",
    }
    assert spec["securityContext"]["runAsNonRoot"] is True
    assert (
        spec["securityContext"]["runAsUser"],
        spec["securityContext"]["runAsGroup"],
    ) == (2000, 3000)
    assert spec["automountServiceAccountToken"] is False
    assert spec["restartPolicy"] == "Never"
    assert spec["imagePullSecrets"] == [{"name": "registry"}]
    assert container["image"] == DEFAULT_IMAGE
    assert container["securityContext"] == {
        "allowPrivilegeEscalation": False,
        "capabilities": {"drop": ["ALL"]},
    }
    assert not {"command", "args", "lifecycle", "envFrom"} & set(container)
    assert container["readinessProbe"] == {
        "tcpSocket": {"port": 2222},
        "periodSeconds": 2,
    }


def test_template_environment_is_passed_as_plain_values_only():
    container = render(environment={"MULTICA_CHECKOUT_PORT": 19514})["spec"][
        "containers"
    ][0]
    assert container["env"] == [{"name": "MULTICA_CHECKOUT_PORT", "value": "19514"}]
