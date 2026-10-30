"""Fixtures for the independently collected system scenarios.

Nothing is created at collection. The k3d cluster is built when the first
test that needs it runs, with its teardown registered immediately; selecting
one scenario provisions only its prerequisites.
"""

import pytest

from tests.support import cluster as cluster_support
from tests.support.ownership import Cleanup


@pytest.fixture(scope="session")
def cluster(tmp_path_factory):
    """One server and two agents, with NetworkPolicy enforcement proven."""
    with Cleanup() as cleanup:
        directory = tmp_path_factory.mktemp("k3d")
        directory.chmod(0o700)
        created = cluster_support.create(2, directory, cleanup)
        cluster_support.import_probe_image(created)
        cluster_support.verify_policy_enforcement(created)
        yield created


@pytest.fixture(scope="session")
def platform(tmp_path_factory):
    """The installed platform with Bob's and Carol's commissioned instances.

    Built once per run on a single-node cluster of its own: node-local storage
    classes and a static local-path source pin every consumer to one node, which
    is the specified single-server setup, and a second node would only make a
    worker that needs both unschedulable. The cases that use it neither tear it
    down nor depend on each other's effects.
    """
    from tests.support import platform as platform_support

    with Cleanup() as cleanup:
        directory = tmp_path_factory.mktemp("platform")
        directory.chmod(0o700)
        built = cluster_support.create(0, directory, cleanup)
        yield platform_support.bring_up(built, directory, cleanup)


def reclaim(platform):
    """Suspends every enabled instance except the commissioned owners'."""
    kept = {receipt["assignmentId"] for receipt in platform.assignments.values()}
    after = ""
    while True:
        page = platform.admin.call(
            "GET", f"/api/v1/assignments?limit=100{after}"
        ).json()
        for item in page["items"]:
            if item["id"] not in kept and item["access"] == "enabled":
                platform.admin.set_access(item["id"], "suspended")
        if not page["nextCursor"]:
            break
        after = f"&after={page['nextCursor']}"


@pytest.fixture(autouse=True)
def reclaimed(request):
    """Stops the instances a case created on the shared platform once it ends.

    Every Hermes instance requests 2 GiB of memory and a CPU, and the shared
    platform is one node, so instances left running by earlier cases starve the
    later ones (about fifteen fill a 31 GB machine). Suspending an instance
    keeps its data and credentials and frees its Pods; the commissioned owners
    stay as they are. The Pods are not waited for: they terminate while the next
    case makes its own people. The destructive cases do the same in their own
    fixture, which owns the platform they share.
    """
    yield
    if "platform" not in request.fixturenames:
        return
    reclaim(request.getfixturevalue("platform"))


@pytest.fixture(scope="session")
def chromium(platform, tmp_path_factory):
    """A real Chromium that trusts the fixture CA and reaches the platform's
    hostnames only; each case takes a context of its own."""
    from tests.personal_hermes import native_e2e as e2e
    from tests.support import browser as browser_support

    home = tmp_path_factory.mktemp("chromium-home")
    home.chmod(0o700)
    node_ip = e2e.container_ip(platform.identifier, platform.network)
    browser = browser_support.Chromium(node_ip, platform.ca, home)
    try:
        yield browser
    finally:
        browser.close()


@pytest.hookimpl(hookwrapper=True)
def pytest_runtest_makereport(item, call):
    """Keeps each phase's report on the item, for the fixtures that ask whether
    the case passed."""
    outcome = yield
    setattr(item, f"report_{call.when}", outcome.get_result())


@pytest.fixture(scope="session")
def destructive_pool(tmp_path_factory):
    """The platform the destructive cases share.

    Building one takes about 3.5 minutes, nearly three quarters of a destructive
    case, so the cases take turns on one. Each case may break it: after every
    case the pool checks that the database, the provider, the platform's
    workloads and the commissioned owners are back, and it replaces the
    platform when they are not or when the case failed. A case that breaks
    something is the last to see it.
    """
    from tests.support import platform as platform_support
    from tests.support.pool import SharedPlatform, unusable

    def build(cleanup):
        directory = tmp_path_factory.mktemp("destructive")
        directory.chmod(0o700)
        built = cluster_support.create(0, directory, cleanup)
        return platform_support.bring_up(built, directory, cleanup)

    pool = SharedPlatform(build, unusable)
    try:
        yield pool
    finally:
        pool.close()


@pytest.fixture
def destructive_platform(request, destructive_pool):
    """The shared platform, for a case that may break it."""
    platform = destructive_pool.take()
    yield platform
    report = getattr(request.node, "report_call", None)
    passed = report is not None and report.passed
    if passed:
        reclaim(platform)
    destructive_pool.release(passed)
