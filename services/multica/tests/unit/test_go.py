"""Run the patched Multica Go unit tests with `go test`, one pytest case per test.

The backend Dockerfile's `test` target runs `go test -json` on the patched
upstream source and exports the report; no Go tool chain is needed on the host.
"""

from __future__ import annotations

import json
import os
import re

import pytest

from agent_platform.build import ROOT, bake_arguments
from agent_platform.command import run
from agent_platform.privacy import stage_contexts
from tests.support.images import CACHE, require_docker

pytestmark = [pytest.mark.toolchain, pytest.mark.image]
SERVICE = ROOT / "services/multica"
PACKAGES = {
    "platformauth": ("auth-boundary_test.go", "maintenance_linux_test.go"),
    "cmd/platform-bootstrap": ("bootstrap/main_test.go",),
}
TEST = re.compile(r"^func (Test\w+)\(t \*testing\.T\)", re.MULTILINE)


def declared() -> list[tuple[str, str]]:
    return [
        (package, name)
        for package, files in PACKAGES.items()
        for file in files
        for name in TEST.findall((SERVICE / file).read_text())
    ]


@pytest.fixture(scope="session")
def go_report(tmp_path_factory) -> dict[tuple[str, str], list[dict]]:
    """Build the `test` target once and group the JSON events by test."""
    require_docker()
    stage = tmp_path_factory.mktemp("multica-go")
    args = bake_arguments(
        github_cache=os.environ.get(CACHE) == "gha",
        cache_write=False,
        contexts=stage_contexts(ROOT, stage / "inputs"),
        targets=("multica-backend",),
    )
    output = stage / "results"
    args += [
        "--allow",
        f"fs={stage}",
        "--set",
        "multica-backend.target=test",
        "--set",
        f"multica-backend.output=type=local,dest={output}",
    ]
    log = stage / "build.log"
    try:
        run(args, cwd=ROOT, timeout=3600, diagnostics=log)
    except RuntimeError as error:
        pytest.fail(f"go test did not run: {error}\n{log.read_text()[-8000:]}")
    events: dict[tuple[str, str], list[dict]] = {}
    for line in (output / "go-test.json").read_text().splitlines():
        if not line.startswith("{"):
            continue
        event = json.loads(line)
        package = event.get("Package", "").split("/server/", 1)[-1]
        events.setdefault((package, event.get("Test", "")), []).append(event)
    events[("", "status")] = [{"Output": (output / "status").read_text().strip()}]
    return events


def output(events: list[dict]) -> str:
    return "".join(event.get("Output", "") for event in events)[-6000:]


@pytest.mark.parametrize(("package", "name"), declared())
def test_go(go_report, package, name):
    events = go_report.get((package, name))
    assert events, f"{package}.{name} did not run:\n{output(go_report[package, ''])}"
    actions = {event["Action"] for event in events}
    assert "pass" in actions and "fail" not in actions, output(events)


def test_go_reports_exactly_the_declared_tests_and_exits_cleanly(go_report):
    """A package that fails to build or an undeclared test fails this case."""
    ran = {key for key in go_report if key[1] and key[0] and "/" not in key[1]}
    assert ran == set(declared())
    status = go_report["", "status"][0]["Output"]
    failures = {
        key: output(events)
        for key, events in go_report.items()
        if any(event.get("Action") == "fail" for event in events)
    }
    assert status == "0", failures
