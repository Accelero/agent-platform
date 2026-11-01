#!/usr/bin/env python3
"""A `kubectl` executable backed by FakeKube, for tests of the command contract.

State lives in the JSON file named by FAKE_KUBE_STATE. Global options the adapter
passes (--kubeconfig, --namespace, ...) are accepted and ignored.
"""

import json
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

from tests.support.fake_kube import FakeKube  # noqa: E402

VALUE_OPTIONS = {"--kubeconfig", "--context", "--cache-dir", "--namespace"}


def main(argv: list[str]) -> int:
    args, it = [], iter(argv)
    for arg in it:
        if arg in VALUE_OPTIONS:
            next(it)
        elif not arg.startswith("--request-timeout"):
            args.append(arg)
    state = Path(os.environ["FAKE_KUBE_STATE"])
    kube = FakeKube(time.time)
    kube.load(state.read_text() if state.exists() else "")
    kube.default_boot = float(os.environ.get("FAKE_KUBE_BOOT", "0"))
    body = json.loads(sys.stdin.read()) if "-f" in args else None
    try:
        result = kube.kubectl(*args, body=body)
    except RuntimeError as error:
        sys.stderr.write(str(error))
        return 1
    state.write_text(kube.dump())
    sys.stdout.write(json.dumps(result) if isinstance(result, dict) else result)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
