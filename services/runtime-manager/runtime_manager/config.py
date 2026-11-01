"""Validated configuration: manager settings and the template catalog."""
from __future__ import annotations

import hashlib
import json
from dataclasses import dataclass

from .domain.model import Mode, Settings, Timers

_SECONDS = {
    'lease_ttl_seconds': ('lease_ttl', 30),
    'max_lease_seconds': ('max_lease', 30),
    'start_timeout_seconds': ('start_timeout', 1),
    'failure_backoff_seconds': ('failure_backoff', 0),
    'drain_deadline_seconds': ('drain_deadline', 1),
    'request_memory_seconds': ('request_memory', 1),
    'orphan_grace_seconds': ('orphan_grace', 0),
    'orphan_scan_seconds': ('orphan_scan', 1),
    'maintenance_interval_seconds': ('maintenance_interval', 1),
}
ORPHAN_POLICIES = ('report', 'delete')


def settings_from(config: dict) -> Settings:
    values = {}
    for key, (field, minimum) in _SECONDS.items():
        if key in config:
            value = config[key]
            if isinstance(value, bool) or not isinstance(value, int) or value < minimum:
                raise ValueError(f'{key} must be an integer of at least {minimum}')
            values[field] = value
    policy = config.get('orphan_policy', 'report')
    if policy not in ORPHAN_POLICIES:
        raise ValueError('orphan_policy must be report or delete')
    settings = Settings(**values, orphan_policy=policy)
    if settings.max_lease < settings.lease_ttl:
        raise ValueError('max_lease_seconds must not be shorter than lease_ttl_seconds')
    return settings


def timers_from(template: dict) -> Timers:
    recycle = template.get('recycle', {})
    return Timers(
        session_idle=template.get('session_idle_seconds', 86400),
        on_demand_idle=template.get('on_demand_idle_seconds', 60),
        inactive_after=recycle.get('inactive_after_seconds', 86400),
        recycle_at=recycle.get('at', ''),
        timezone=recycle.get('timezone', 'Europe/Berlin'))


@dataclass(frozen=True, slots=True)
class Resolved:
    template: dict
    fingerprint: str
    timers: Timers


class Catalog:
    """The deployments and templates the chart configured."""

    def __init__(self, config: dict):
        self.config = config
        self.default = config.get('default_template', 'hermes')

    def resolve(self, deployment, name, image=None) -> Resolved:
        if deployment not in self.config.get('deployments', {}):
            raise ValueError('Unknown deployment')
        if name not in self.config.get('templates', {}):
            raise ValueError('Unknown terminal template or lifetime mode')
        template = dict(self.config['templates'][name])
        if image is not None:
            if (not isinstance(image, str) or not image or len(image) > 512
                    or any(c.isspace() or ord(c) < 32 or ord(c) == 127 for c in image)):
                raise ValueError('Invalid terminal image')
            template['image'] = image
        # The fingerprint formula is part of the stored state: changing it would
        # replace every running worker once.
        fingerprint = hashlib.sha256(json.dumps(template, sort_keys=True).encode()).hexdigest()
        return Resolved(template, fingerprint, timers_from(template))


def mode_of(value) -> Mode:
    if not isinstance(value, str) or value not in {m.value for m in Mode}:
        raise ValueError('Invalid scope lifetime')
    return Mode(value)
