"""A private home with a damaged initialization record (R8, R19).

The image's initializer proves a home is the owner's with a signed intent and
receipt in `.agent-platform/`. Whatever happens to them, a home that holds user
data is never overwritten or reseeded, and Hermes never starts on a home whose
proof does not verify:

- a receipt with a flipped signature: Hermes does not start; with the genuine
  receipt back it does, on the same data;
- a missing receipt with its intact intent: the initializer finishes what the
  intent records and writes the very same receipt, and the data is untouched;
- a missing receipt and intent on a home that holds data: the initializer
  refuses to adopt it; with both back, Hermes starts on the same data.

The instance is reported as blocked with the typed reason
`invalid_initialization_receipt` while its initializer refuses the home, and
starts by itself once the proof is back. The files are changed on the node, where
the volume lives, because nothing can be executed in a Pod that cannot start.
"""

import hashlib
import json
import secrets
import subprocess
import time

import pytest

from tests.personal_hermes import native_e2e as scenarios
from tests.support.people import person, ready

pytestmark = [
    pytest.mark.e2e,
    pytest.mark.cluster,
    pytest.mark.slow,
    pytest.mark.security,
]

STORAGE = "/var/lib/rancher/k3s/storage"
HOLD = 45


def node(platform, script, data=None):
    result = subprocess.run(
        ["docker", "exec", "-i", platform.identifier, "sh", "-c", script],
        input=data,
        capture_output=True,
        timeout=60,
        check=True,
    )
    return result.stdout


class Home:
    """One instance's private home, reached through the node's storage."""

    def __init__(self, platform, namespace):
        self.platform = platform
        found = node(
            platform,
            f"find {STORAGE} -maxdepth 1 -name '*{namespace}_hermes-home'",
        ).split()
        assert len(found) == 1, found
        self.root = found[0].decode()
        self.meta = f"{self.root}/.agent-platform"

    def read(self, name):
        return node(self.platform, f"cat {self.root}/{name}")

    def write(self, name, data):
        node(self.platform, f"cat > {self.root}/{name}", data=data)

    def remove(self, *names):
        node(self.platform, "rm -f " + " ".join(f"{self.root}/{n}" for n in names))

    def digest(self, *names):
        return {n: hashlib.sha256(self.read(n)).hexdigest() for n in names}


def stop(platform, receipt):
    """Suspends the instance until it is retained: no Pod, the home at rest."""
    platform.admin.set_access(receipt["assignmentId"], "suspended")
    scenarios.wait(
        lambda: platform.admin.phase(receipt["instanceId"])["phase"] == "retained",
        300,
        "the instance was not retained",
        interval=5,
    )


def start(platform, receipt):
    platform.admin.set_access(receipt["assignmentId"], "enabled")


def blocked_and_unstarted(platform, receipt, namespace):
    """The controller reports the instance blocked with the typed reason, and
    over the next HOLD seconds it stays blocked and its Hermes container never
    starts."""
    admin = platform.admin
    scenarios.wait(
        lambda: admin.phase(receipt["instanceId"])["phase"] == "blocked",
        240,
        lambda: admin.phase(receipt["instanceId"]),
        interval=3,
    )
    deadline = time.monotonic() + HOLD
    while time.monotonic() < deadline:
        phase = admin.phase(receipt["instanceId"])
        assert phase["phase"] == "blocked", phase
        assert phase["blockedReason"] == "invalid_initialization_receipt", phase
        started = scenarios.kubectl(
            platform.identifier,
            "get",
            "pods",
            "--namespace",
            namespace,
            "-o",
            "jsonpath={.items[*].status.containerStatuses[*].state.running}",
        )
        assert not started.strip(), "Hermes started on a home that does not verify"
        time.sleep(5)


def test_a_damaged_initialization_record_never_lets_hermes_start_or_changes_the_home(
    platform,
):
    receipt = person(
        platform,
        "home",
        {"template": {"id": "personal", "version": platform.state.template}},
    )
    namespace = ready(platform, receipt)
    scenarios.instance_pod_exec(
        platform.identifier,
        namespace,
        f"echo {secrets.token_hex(8)} > /opt/data/e2e-marker",
    )
    home = Home(platform, namespace)
    data = ("e2e-marker", "config.yaml", "SOUL.md")
    before = home.digest(*data)
    receipt_name = ".agent-platform/native-init-receipt.json"
    intent_name = ".agent-platform/native-init-intent.json"
    genuine, intent = home.read(receipt_name), home.read(intent_name)

    def started_again():
        start(platform, receipt)
        ready(platform, receipt)
        assert home.digest(*data) == before, "the home's data changed"
        assert home.read(receipt_name) == genuine, "the receipt is not the genuine one"

    # A receipt whose signature is wrong: Hermes does not start on it.
    stop(platform, receipt)
    forged = json.loads(genuine)
    forged["signature"] = forged["signature"][:-1] + (
        "0" if forged["signature"][-1] != "0" else "1"
    )
    home.write(receipt_name, json.dumps(forged).encode())
    start(platform, receipt)
    blocked_and_unstarted(platform, receipt, namespace)
    assert home.digest(*data) == before, "a damaged receipt changed the home"
    stop(platform, receipt)
    home.write(receipt_name, genuine)
    started_again()

    # No receipt, intent intact: the initializer finishes what the intent
    # records, and the receipt it writes is the same one.
    stop(platform, receipt)
    home.remove(receipt_name)
    started_again()

    # Neither receipt nor intent on a home that holds data: it is not adopted.
    stop(platform, receipt)
    home.remove(receipt_name, intent_name)
    start(platform, receipt)
    blocked_and_unstarted(platform, receipt, namespace)
    assert home.digest(*data) == before, "an unproven home was changed"
    stop(platform, receipt)
    home.write(receipt_name, genuine)
    home.write(intent_name, intent)
    started_again()
