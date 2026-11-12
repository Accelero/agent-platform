"""One installation lease shared by bootstrap, recovery and deployment seed Jobs."""

from __future__ import annotations

import os
from collections.abc import Iterator
from contextlib import contextmanager
from datetime import UTC, datetime

import kubernetes.client as kube

REQUEST_TIMEOUT = (5, 30)
NOT_FOUND = 404


@contextmanager
def operation_lock(
    coordination: kube.CoordinationV1Api,
    namespace: str,
    name: str = "agent-state-operation",
) -> Iterator[None]:
    """Bound lease ownership beyond the maximum lifetime of its Job."""
    now = datetime.now(UTC)
    identity = os.environ["POD_UID"]
    lease = kube.V1Lease(
        metadata=kube.V1ObjectMeta(name=name),
        spec=kube.V1LeaseSpec(
            holder_identity=identity,
            acquire_time=now,
            renew_time=now,
            lease_duration_seconds=1500,
        ),
    )
    try:
        existing = coordination.read_namespaced_lease(
            name,
            namespace,
            _request_timeout=REQUEST_TIMEOUT,
        )
    except kube.ApiException as error:
        if error.status != NOT_FOUND:
            raise
        lease = coordination.create_namespaced_lease(
            namespace,
            lease,
            _request_timeout=REQUEST_TIMEOUT,
        )
    else:
        if existing.spec.holder_identity == identity:
            # The chart bootstrap owns the outer lease in this same Pod.
            # Its checkpoint subprocess must not release that parent's lease.
            yield
            return
        start = existing.spec.renew_time or existing.spec.acquire_time
        if (
            start is None
            or (now - start).total_seconds() <= existing.spec.lease_duration_seconds
        ):
            msg = "Another agent state operation holds the installation lock."
            raise RuntimeError(msg)
        lease.metadata.resource_version = existing.metadata.resource_version
        lease = coordination.replace_namespaced_lease(
            name,
            namespace,
            lease,
            _request_timeout=REQUEST_TIMEOUT,
        )
    try:
        yield
    finally:
        coordination.delete_namespaced_lease(
            name,
            namespace,
            body=kube.V1DeleteOptions(
                preconditions=kube.V1Preconditions(uid=lease.metadata.uid),
            ),
            _request_timeout=REQUEST_TIMEOUT,
        )
