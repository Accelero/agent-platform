"""Assign every collected test to one suite: one component at one level.

The directory sets the level. A suite is `<component>-<level>`, or the global
`e2e`. Each suite runs on its own CI runner; see docs/testing.md. The command
line lists suites and expands a selection into the GitHub Actions job matrix:

    python -m tests.support.suites list
    python -m tests.support.suites matrix --select all
"""

from __future__ import annotations

import argparse
import json
import os
import socket
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
LEVELS = ("unit", "integration", "e2e")


@dataclass(frozen=True)
class Suite:
    """One CI runner: a test directory and what that runner must provide."""

    name: str
    path: str
    docker: bool = False
    helm: bool = False
    shards: int = 1
    timeout: int = 15

    @property
    def level(self) -> str:
        return Path(self.path).name


SUITES = (
    Suite("platform-unit", "agent_platform/tests/unit"),
    Suite("platform-integration", "agent_platform/tests/integration", helm=True),
    Suite("chart-integration", "charts/tests/integration", docker=True, helm=True),
    Suite("manager-unit", "services/runtime-manager/tests/unit"),
    Suite(
        "manager-integration",
        "services/runtime-manager/tests/integration",
        docker=True,
        helm=True,
        timeout=30,
    ),
    Suite("tools-unit", "services/tools/tests/unit"),
    Suite(
        "tools-integration",
        "services/tools/tests/integration",
        docker=True,
        helm=True,
        timeout=45,
    ),
    Suite("multica-unit", "services/multica/tests/unit", docker=True, timeout=45),
    Suite(
        "multica-integration",
        "services/multica/tests/integration",
        docker=True,
        timeout=45,
    ),
    Suite("hermes-unit", "runtimes/hermes/tests/unit"),
    Suite(
        "hermes-integration",
        "runtimes/hermes/tests/integration",
        docker=True,
        timeout=45,
    ),
    Suite(
        "worker-integration",
        "runtimes/agent-runtime/tests/integration",
        docker=True,
        timeout=30,
    ),
    Suite("e2e", "tests/e2e", docker=True, helm=True, timeout=60),
)
BY_NAME = {suite.name: suite for suite in SUITES}
SUITE = pytest.StashKey[Suite]()


def suite_of(path: Path) -> Suite | None:
    for suite in SUITES:
        if path.is_relative_to(ROOT / suite.path):
            return suite
    return None


def select(value: str) -> list[Suite]:
    """Resolve `all` or a comma-separated list of suite names."""
    if value.strip() == "all":
        return list(SUITES)
    names = [name.strip() for name in value.split(",") if name.strip()]
    unknown = [name for name in names if name not in BY_NAME]
    if unknown or not names:
        raise ValueError(
            "Unknown suite selection "
            + ", ".join(unknown or [repr(value)])
            + "; choose from "
            + ", ".join(BY_NAME)
        )
    return [BY_NAME[name] for name in dict.fromkeys(names)]


def shard(value: str) -> tuple[int, int]:
    try:
        index, total = (int(part) for part in value.split("/"))
    except ValueError:
        raise argparse.ArgumentTypeError("use I/N, for example 2/3") from None
    if not 1 <= index <= total:
        raise argparse.ArgumentTypeError("use 1 <= I <= N")
    return index, total


def pytest_addoption(parser: pytest.Parser) -> None:
    group = parser.getgroup("suites", "component suites (docs/testing.md)")
    group.addoption(
        "--suite",
        help="run only these comma-separated suites, for example manager-unit",
    )
    group.addoption(
        "--shard",
        type=shard,
        help="run only part I of N of the selected tests, in collection order",
    )
    group.addoption(
        "--manual",
        action="store_true",
        help="also run `manual` tests, which need credentials the repository lacks",
    )
    group.addoption(
        "--check-suites",
        action="store_true",
        help="fail unless every registered suite collected at least one test",
    )


def pytest_configure(config: pytest.Config) -> None:
    for line in (
        "unit: set from the directory; in process, no subprocess or network socket",
        "integration: set from the directory; one component across a real boundary",
        "e2e: set from the directory; several components together",
        "image: builds or runs a container image",
        "artifact: verifies a release image; rerun on the pushed release digests",
        "manual: needs credentials the repository lacks; selected by --manual",
        "toolchain: a unit test run by another language's tool chain",
    ):
        config.addinivalue_line("markers", line)
    config.pluginmanager.register(_Shard(), "suite-shard")


@pytest.hookimpl(tryfirst=True)
def pytest_collection_modifyitems(
    session: pytest.Session, config: pytest.Config, items: list[pytest.Item]
) -> None:
    """Mark levels before `-m` runs, then narrow to the requested suites."""
    problems = []
    for item in items:
        suite = suite_of(Path(item.path).resolve())
        if suite is None:
            problems.append(
                f"{item.nodeid}: not inside a registered suite directory "
                "(see tests/support/suites.py)"
            )
            continue
        if any(item.get_closest_marker(name) for name in LEVELS):
            problems.append(f"{item.nodeid}: its directory sets the level; remove it")
            continue
        item.stash[SUITE] = suite
        item.add_marker(suite.level)
    if problems:
        raise pytest.UsageError("\n".join(problems))
    if config.getoption("check_suites"):
        empty = {suite.name for suite in SUITES} - {
            item.stash[SUITE].name for item in items
        }
        if empty:
            raise pytest.UsageError("Suites without tests: " + ", ".join(sorted(empty)))
    keep, drop = [], []
    wanted = (
        {suite.name for suite in select(config.getoption("suite"))}
        if config.getoption("suite")
        else None
    )
    for item in items:
        if wanted is not None and item.stash[SUITE].name not in wanted:
            drop.append(item)
        elif item.get_closest_marker("manual") and not config.getoption("manual"):
            drop.append(item)
        else:
            keep.append(item)
    if drop:
        config.hook.pytest_deselected(items=drop)
        items[:] = keep


class _Shard:
    """Keep one contiguous part after every other deselection has run."""

    @pytest.hookimpl(trylast=True)
    def pytest_collection_modifyitems(
        self, config: pytest.Config, items: list[pytest.Item]
    ) -> None:
        selected = config.getoption("shard")
        if not selected:
            return
        index, total = selected
        start, end = (len(items) * (index - 1)) // total, (len(items) * index) // total
        config.hook.pytest_deselected(items=items[:start] + items[end:])
        items[:] = items[start:end]


class BoundaryError(AssertionError):
    """A unit test reached outside its process."""


def _refuse(what: str):
    def refuse(*args, **kwargs):
        raise BoundaryError(
            f"unit tests may not {what}; move this test to integration/"
        )

    return refuse


def _inet(sock: socket.socket) -> bool:
    return sock.family in (socket.AF_INET, socket.AF_INET6)


def _guarded(item: pytest.Item):
    if not item.get_closest_marker("unit") or item.get_closest_marker("toolchain"):
        return None
    patch = pytest.MonkeyPatch()
    patch.setattr(subprocess.Popen, "__init__", _refuse("start a process"))
    for name in ("system", "fork", "posix_spawn", "posix_spawnp"):
        if hasattr(os, name):
            patch.setattr(os, name, _refuse("start a process"))
    for name in ("connect", "connect_ex", "bind"):
        original = getattr(socket.socket, name)

        def guarded(self, *args, _original=original, **kwargs):
            if _inet(self):
                raise BoundaryError(
                    "unit tests may not open a network socket; "
                    "move this test to integration/"
                )
            return _original(self, *args, **kwargs)

        patch.setattr(socket.socket, name, guarded)
    return patch


@pytest.hookimpl(wrapper=True)
def pytest_runtest_setup(item: pytest.Item):
    patch = _guarded(item)
    try:
        return (yield)
    finally:
        if patch:
            patch.undo()


@pytest.hookimpl(wrapper=True)
def pytest_runtest_call(item: pytest.Item):
    patch = _guarded(item)
    try:
        return (yield)
    finally:
        if patch:
            patch.undo()


@pytest.hookimpl(wrapper=True)
def pytest_runtest_makereport(item: pytest.Item, call: pytest.CallInfo):
    """In CI a selected test must run: report a skip as a failure."""
    report = yield
    if (
        os.environ.get("CI")
        and report.skipped
        and not hasattr(report, "wasxfail")
        and item.get_closest_marker("manual") is None
    ):
        reason = report.longrepr[2] if isinstance(report.longrepr, tuple) else ""
        report.outcome = "failed"
        report.longrepr = f"Skipped in CI, where every selected test must run: {reason}"
    return report


def matrix(suites: list[Suite]) -> dict:
    include = []
    for suite in suites:
        for index in range(1, suite.shards + 1):
            include.append(
                {
                    "name": suite.name
                    if suite.shards == 1
                    else f"{suite.name} ({index}/{suite.shards})",
                    "suite": suite.name,
                    "shard": f"{index}/{suite.shards}",
                    "docker": suite.docker,
                    "helm": suite.helm,
                    "timeout": suite.timeout,
                }
            )
    return {"include": include}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="python -m tests.support.suites")
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("list", help="show suites and their runner needs")
    expand = commands.add_parser("matrix", help="print a GitHub Actions job matrix")
    expand.add_argument("--select", default="all")
    args = parser.parse_args(argv)
    if args.command == "list":
        for suite in SUITES:
            needs = [name for name in ("docker", "helm") if getattr(suite, name)]
            print(
                f"{suite.name:22} {suite.path:44} shards={suite.shards} "
                f"timeout={suite.timeout}m {' '.join(needs)}"
            )
        return 0
    try:
        selected = select(args.select)
    except ValueError as error:
        parser.error(str(error))
    print(json.dumps(matrix(selected), separators=(",", ":")))
    return 0


if __name__ == "__main__":
    sys.exit(main())
