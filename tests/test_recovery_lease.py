"""Keep nested bootstrap/recovery commands under one installation lease."""

from datetime import UTC, datetime
from unittest.mock import MagicMock, patch

import pytest
from platform_recovery.lease import operation_lock

pytestmark = pytest.mark.unit


def test_checkpoint_child_reuses_only_its_own_pod_lease() -> None:
    """Nested checkpoint commands cannot release the bootstrap parent's lease."""
    cluster = MagicMock()
    cluster.namespace = "agents"
    cluster.coordination.read_namespaced_lease.return_value.spec.holder_identity = "pod"
    with (
        patch.dict("os.environ", {"POD_UID": "pod"}),
        operation_lock(cluster.coordination, cluster.namespace),
    ):
        pass
    cluster.coordination.create_namespaced_lease.assert_not_called()
    cluster.coordination.replace_namespaced_lease.assert_not_called()
    cluster.coordination.delete_namespaced_lease.assert_not_called()


def test_checkpoint_child_cannot_bypass_another_pods_lease() -> None:
    """A different live holder remains a fail-closed backup exclusion."""
    cluster = MagicMock()
    cluster.namespace = "agents"
    lease = cluster.coordination.read_namespaced_lease.return_value
    lease.spec.holder_identity = "other"
    lease.spec.renew_time = datetime.now(UTC)
    lease.spec.lease_duration_seconds = 1500
    with (
        patch.dict("os.environ", {"POD_UID": "pod"}),
        pytest.raises(RuntimeError, match="installation lock"),
        operation_lock(cluster.coordination, cluster.namespace),
    ):
        pass
    cluster.coordination.delete_namespaced_lease.assert_not_called()


@pytest.mark.parametrize("race", [False, True])
def test_waiting_bootstrap_retries_a_held_lease_or_create_race(race):
    import kubernetes.client as kube

    coordination = MagicMock()
    held = MagicMock()
    held.spec.holder_identity = "other"
    held.spec.renew_time = datetime.now(UTC)
    held.spec.lease_duration_seconds = 1500
    missing = kube.ApiException(status=404)
    if race:
        coordination.read_namespaced_lease.side_effect = [missing, missing]
        coordination.create_namespaced_lease.side_effect = [
            kube.ApiException(status=409),
            MagicMock(),
        ]
    else:
        coordination.read_namespaced_lease.side_effect = [held, missing]
    with (
        patch.dict("os.environ", {"POD_UID": "pod"}),
        patch("platform_recovery.lease.time.sleep") as sleep,
        operation_lock(coordination, "agents", wait_seconds=180),
    ):
        pass
    sleep.assert_called_once()
    options = coordination.delete_namespaced_lease.call_args.kwargs["body"]
    assert options.preconditions.resource_version is not None
