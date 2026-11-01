"""Lease traffic and other profiles remain usable during cluster I/O (X16)."""

import threading
from concurrent.futures import ThreadPoolExecutor

import pytest

from tests.support.http import serve
from tests.support.world import CUSTOM_IMAGE

pytestmark = pytest.mark.integration


def test_tls_lease_traffic_and_other_profiles_do_not_wait_for_cluster_io(world, tls):
    entered, proceed = threading.Event(), threading.Event()
    kubectl = world.cluster.kubectl

    def slow(*args, body=None):
        if (
            args[0] == "create"
            and body["spec"]["containers"][0]["image"] == CUSTOM_IMAGE
        ):
            entered.set()
            assert proceed.wait(5)
        return kubectl(*args, body=body)

    world.cluster.kubectl = slow
    with serve(world, tls) as http, ThreadPoolExecutor(max_workers=1) as pool:
        acquire = {
            "profile": "steady",
            "session": "s",
            "request": "steady",
            "mode": "per_profile_on_demand",
        }
        status, _, held = http.call("POST", "/v1/leases/acquire", acquire)
        assert status == 200
        blocked = pool.submit(
            http.call,
            "POST",
            "/v1/leases/acquire",
            {**acquire, "profile": "slow", "request": "slow", "image": CUSTOM_IMAGE},
        )
        try:
            assert entered.wait(5)
            assert (
                http.call("POST", "/v1/leases/heartbeat", {"lease": held["lease"]})[0]
                == 200
            )
            assert http.call("GET", "/v1/environments")[0] == 200
            assert (
                http.call(
                    "POST",
                    "/v1/leases/acquire",
                    {**acquire, "profile": "other", "request": "other"},
                )[0]
                == 200
            )
            assert (
                http.call("POST", "/v1/leases/release", {"lease": held["lease"]})[0]
                == 200
            )
            assert not blocked.done()
        finally:
            proceed.set()
        assert blocked.result(timeout=5)[0] == 200
