"""Failure scenarios: they pause the database, scale the manager or suspend
owners, so they are `destructive` and run on the destructive platform, which
the next case gets only if this one left it healthy (`tests/support/pool.py`)."""

import pytest

from tests.personal_hermes import native_e2e as scenarios

pytestmark = [
    pytest.mark.e2e,
    pytest.mark.cluster,
    pytest.mark.destructive,
    pytest.mark.slow,
    pytest.mark.security,
]


def test_outages_and_provider_revocation_fail_closed_within_the_bounds(
    destructive_platform,
):
    platform = destructive_platform
    scenarios.outages_and_provider_revocation_fail_closed_within_the_bounds(
        platform.identifier,
        platform.postgres,
        platform.admin,
        platform.ca,
        platform.passwords,
        platform.assignments,
    )


def test_suspension_closes_open_connections_and_reenabling_keeps_data(
    destructive_platform,
):
    platform = destructive_platform
    scenarios.suspension_closes_open_connections_and_reenabling_keeps_data(
        platform.identifier,
        platform.admin,
        platform.ca,
        platform.passwords,
        platform.assignments,
    )
