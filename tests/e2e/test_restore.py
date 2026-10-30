"""Restoring PostgreSQL and the storage root onto a second, empty cluster.

The scenario archives the first cluster's data, builds a second cluster, runs
`rebind-cluster` and also checks, on the restored installation, that an
instance missing from an older backup is reported as an orphan and never
adopted. Both claims are asserted inside this one case.
"""

import pytest

from tests.personal_hermes import native_e2e as scenarios

pytestmark = [
    pytest.mark.e2e,
    pytest.mark.cluster,
    pytest.mark.destructive,
    pytest.mark.slow,
]


def test_restore_on_a_new_cluster_rebinds_without_adoption(destructive_platform):
    platform = destructive_platform
    scenarios.restore_on_a_new_cluster_rebinds_without_adoption(
        platform.identifier,
        platform.postgres,
        platform.network,
        platform.database,
        str(platform.directory),
        platform.tls,
        platform.ca,
        platform.passwords,
        platform.owned,
        platform.assignments,
        platform.cluster.run,
    )
