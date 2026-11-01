"""Typed errors. Each carries the HTTP status and stable code of the contract."""
from __future__ import annotations


class ManagerError(Exception):
    status = 503
    code = 'unavailable'
    retry_after: int | None = None
    reason: str | None = None


class Invalid(ManagerError, ValueError):
    status = 400
    code = 'invalid_request'


class Unauthorized(ManagerError):
    status = 401
    code = 'unauthorized'


class Forbidden(ManagerError):
    status = 403
    code = 'forbidden'


class NotFound(ManagerError):
    status = 404
    code = 'not_found'


class Conflict(ManagerError):
    status = 409
    code = 'conflict'


class Pending(Conflict):
    """A Pod was requested and is not Ready yet."""
    code = 'environment_pending'
    retry_after = 1


class Draining(Conflict):
    """A replacement is due and leases of the current Pod are still live."""
    code = 'environment_draining'
    retry_after = 1


class LeaseNotLive(Conflict):
    """The request's lease already ended; it never grants another."""
    code = 'lease_not_live'


class LeaseExpired(Conflict):
    code = 'lease_expired'


class BackendFailed(ManagerError):
    """The worker cannot start. `reason` is the recorded failure."""
    status = 422
    code = 'backend_failed'

    def __init__(self, reason: str):
        super().__init__(f'Worker could not start: {reason}')
        self.reason = reason
