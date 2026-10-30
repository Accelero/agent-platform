"""Terminal backend lifecycle on a platform of its own: the scenario changes
the owner's backend versions, so it is `destructive`."""

import pytest

from tests.personal_hermes import native_e2e as scenarios

pytestmark = [
    pytest.mark.e2e,
    pytest.mark.cluster,
    pytest.mark.destructive,
    pytest.mark.slow,
]


def test_a_backend_change_keeps_active_leases_and_drains_superseded_environments(
    destructive_platform,
):
    platform = destructive_platform
    scenarios.a_backend_change_keeps_active_leases_and_drains_superseded_environments(
        platform.identifier,
        platform.admin,
        platform.assignments,
        platform.state.worker,
        platform.state.versions,
    )
