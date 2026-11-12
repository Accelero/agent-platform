"""Share one absolute deadline across capture, transport and payload hashing."""

from __future__ import annotations

import hashlib
import os
import time
from collections.abc import Callable
from datetime import UTC, datetime
from pathlib import Path

import kubernetes.client as kube
import kubernetes.config
import urllib3

_NETWORK_ERRORS = (urllib3.exceptions.HTTPError, OSError)

_DEADLINE: float | None = None
OPERATION_SECONDS = 1080
JOB_SECONDS = 1200
CLEANUP_SECONDS = 90


def configure(deadline: float | None) -> None:
    global _DEADLINE
    _DEADLINE = deadline


def remaining(limit: float = OPERATION_SECONDS) -> float:
    seconds = limit if _DEADLINE is None else min(limit, _DEADLINE - time.monotonic())
    if seconds <= 0:
        raise TimeoutError("Recovery operation exhausted its time budget.")
    return seconds


def read_identity(
    operation: Callable[[], kube.V1Pod | kube.V1Job],
) -> kube.V1Pod | kube.V1Job:
    """Retry transient startup networking; preserve other failures."""
    deadline = time.monotonic() + 30
    while True:
        try:
            return operation()
        except kube.ApiException as error:
            if error.status not in (0, 502, 503, 504):
                raise
        except _NETWORK_ERRORS:
            pass
        if time.monotonic() >= deadline:
            raise TimeoutError("The Job identity API did not become reachable.")
        time.sleep(1)


def for_job(config: dict) -> dict:
    """Use the owning Job's actual start, including bootstrap work and lease waits."""
    kubernetes.config.load_incluster_config()
    pod = read_identity(
        lambda: kube.CoreV1Api().read_namespaced_pod(
            os.environ["POD_NAME"], config["namespace"], _request_timeout=(5, 10)
        )
    )
    owners = [
        owner
        for owner in pod.metadata.owner_references
        if owner.kind == "Job" and owner.controller
    ]
    if pod.metadata.uid != os.environ["POD_UID"] or len(owners) != 1:
        raise RuntimeError("Recovery Pod identity is invalid.")
    job = read_identity(
        lambda: kube.BatchV1Api().read_namespaced_job(
            owners[0].name, config["namespace"], _request_timeout=(5, 10)
        )
    )
    if (
        job.metadata.uid != owners[0].uid
        or job.status.start_time is None
        or not 0 < job.spec.active_deadline_seconds <= JOB_SECONDS
    ):
        raise RuntimeError("Recovery requires a matching bounded administrator Job.")
    elapsed = (datetime.now(UTC) - job.status.start_time).total_seconds()
    seconds = min(
        OPERATION_SECONDS, job.spec.active_deadline_seconds - elapsed - CLEANUP_SECONDS
    )
    deadline = min(
        config.get("_operation_deadline", float("inf")), time.monotonic() + seconds
    )
    configure(deadline)
    remaining()
    return {**config, "_operation_deadline": deadline}


def file_sha256(path: Path) -> str:
    """Check the deadline between bounded reads, including large individual files."""
    result = hashlib.sha256()
    with path.open("rb") as source:
        while True:
            remaining()
            block = source.read(8 * 1024 * 1024)
            if not block:
                break
            result.update(block)
    return result.hexdigest()
