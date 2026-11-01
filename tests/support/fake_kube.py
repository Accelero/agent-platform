"""A simulated execution namespace that answers the `kubectl` calls of the cluster
adapter.

Pods follow a scenario and a clock: they become Ready after `boot` seconds, can be
made to fail their image pull or crash, and disappear a configurable time after
deletion. The adapter's real rendering and parsing run against it, so a test
exercises the same JSON the cluster would produce.
"""

import json
from datetime import UTC, datetime

NAMESPACE = "hermes-execution"


class FakeKube:
    def __init__(self, clock, *, namespace: str = NAMESPACE):
        self.clock = clock
        self.namespace = namespace
        self.pods: dict[str, dict] = {}
        self.boot: dict[str, float] = {}  # image -> seconds until Ready
        self.default_boot = 0.0
        self.pull_errors: set[str] = set()  # images that cannot be pulled
        self.crashing: set[str] = set()  # images whose container exits
        self.unschedulable = False
        self.terminate_after = 0.0  # seconds between delete and absence
        self.calls: list[tuple] = []

    # kubectl -------------------------------------------------------------------

    def kubectl(self, *args, body=None):
        self.calls.append(args)
        verb = args[0]
        if verb == "create":
            return self._create(body)
        if verb == "get" and args[1] == "pod":
            pod = self._pod(args[2])
            return self._view(pod) if pod else ""
        if verb == "get" and args[1] == "pods":
            selector = args[args.index("-l") + 1]
            key, _, value = selector.partition("=")
            return {
                "items": [
                    self._view(pod)
                    for name in list(self.pods)
                    if (pod := self._pod(name))
                    and pod["manifest"]["metadata"]["labels"].get(key) == value
                ]
            }
        if verb == "delete":
            pod = self._pod(args[2])
            if pod and pod["deleted"] is None:
                pod["deleted"] = self.clock()
            return ""
        raise AssertionError(f"unsupported kubectl call: {args}")

    # Scenario controls ---------------------------------------------------------

    def add(self, manifest: dict, *, created: float | None = None) -> None:
        """A Pod that already exists, for example one left over from another manager."""
        self.pods[manifest["metadata"]["name"]] = {
            "manifest": manifest,
            "created": self.clock() if created is None else created,
            "deleted": None,
            "failed": False,
        }

    def vanish(self, name: str) -> None:
        """The Pod is deleted outside the manager."""
        self.pods.pop(name, None)

    def fail(self, name: str) -> None:
        """The Pod's container exits."""
        self.pods[name]["failed"] = True

    def images(self) -> list[str]:
        return [
            pod["manifest"]["spec"]["containers"][0]["image"]
            for pod in self.pods.values()
        ]

    def created(self) -> list[tuple]:
        return [call for call in self.calls if call[0] == "create"]

    def deleted(self) -> list[str]:
        """Names of Pods whose deletion was requested."""
        return [call[2] for call in self.calls if call[0] == "delete"]

    # Internals -----------------------------------------------------------------

    def _create(self, manifest):
        name = manifest["metadata"]["name"]
        if name in self.pods:
            raise RuntimeError(f'pods "{name}" already exists (AlreadyExists)')
        self.add(manifest)
        return self._view(self.pods[name])

    def _pod(self, name):
        pod = self.pods.get(name)
        if pod and pod["deleted"] is not None:
            if self.clock() - pod["deleted"] >= self.terminate_after:
                del self.pods[name]
                return None
        return pod

    def _view(self, pod: dict) -> dict:
        manifest = pod["manifest"]
        image = manifest["spec"]["containers"][0]["image"]
        age = self.clock() - pod["created"]
        metadata = {
            **manifest["metadata"],
            "creationTimestamp": datetime.fromtimestamp(pod["created"], UTC)
            .isoformat()
            .replace("+00:00", "Z"),
        }
        if pod["deleted"] is not None:
            metadata["deletionTimestamp"] = "2026-01-01T00:00:00Z"
        status: dict = {"phase": "Pending"}
        if pod["failed"] or image in self.crashing:
            status = {
                "phase": "Failed",
                "containerStatuses": [
                    {"state": {"terminated": {"exitCode": 1, "reason": "Error"}}}
                ],
            }
        elif image in self.pull_errors:
            status["containerStatuses"] = [
                {"state": {"waiting": {"reason": "ImagePullBackOff"}}}
            ]
        elif self.unschedulable:
            status["conditions"] = [
                {"type": "PodScheduled", "status": "False", "reason": "Unschedulable"}
            ]
        elif age >= self.boot.get(image, self.default_boot):
            status = {
                "phase": "Running",
                "podIP": "10.42.0."
                + str(1 + len(pod["manifest"]["metadata"]["name"]) % 200),
                "conditions": [{"type": "Ready", "status": "True"}],
            }
        else:
            status["containerStatuses"] = [
                {"state": {"waiting": {"reason": "ContainerCreating"}}}
            ]
        return {**manifest, "metadata": metadata, "status": status}

    # Persistence, for the standalone executable --------------------------------

    def dump(self) -> str:
        return json.dumps({"pods": self.pods})

    def load(self, text: str) -> None:
        self.pods = json.loads(text)["pods"] if text else {}
