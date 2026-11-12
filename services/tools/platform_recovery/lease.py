"""One installation lease shared by bootstrap, recovery and deployment seed Jobs."""

from __future__ import annotations

import os
import time
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
    *,
    wait_seconds: float = 0,
) -> Iterator[None]:
    """Acquire with bounded CAS retries; same-Pod children never release the parent."""
    identity = os.environ["POD_UID"]
    deadline = time.monotonic() + wait_seconds
    while True:
        now = datetime.now(UTC)
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
                name, namespace, _request_timeout=REQUEST_TIMEOUT
            )
        except kube.ApiException as error:
            if error.status != NOT_FOUND:
                raise
            existing = None
        if existing is not None and existing.spec.holder_identity == identity:
            yield
            return
        start = (
            None
            if existing is None
            else existing.spec.renew_time or existing.spec.acquire_time
        )
        held = existing is not None and (
            start is None
            or (now - start).total_seconds() <= existing.spec.lease_duration_seconds
        )
        if not held:
            try:
                if existing is None:
                    lease = coordination.create_namespaced_lease(
                        namespace, lease, _request_timeout=REQUEST_TIMEOUT
                    )
                else:
                    lease.metadata.resource_version = existing.metadata.resource_version
                    lease = coordination.replace_namespaced_lease(
                        name, namespace, lease, _request_timeout=REQUEST_TIMEOUT
                    )
                break
            except kube.ApiException as error:
                if error.status != 409:
                    raise
        if time.monotonic() >= deadline:
            raise RuntimeError(
                "Another agent state operation holds the installation lock."
            )
        time.sleep(min(2, max(0, deadline - time.monotonic())))
    try:
        yield
    finally:
        coordination.delete_namespaced_lease(
            name,
            namespace,
            body=kube.V1DeleteOptions(
                preconditions=kube.V1Preconditions(
                    uid=lease.metadata.uid,
                    resource_version=lease.metadata.resource_version,
                )
            ),
            _request_timeout=REQUEST_TIMEOUT,
        )
