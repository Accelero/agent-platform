"""Inventory and verify collected Python and Rust cases against a reviewed ledger.

Reports contain case identities and outcomes, never captured output, credentials,
exception messages or environments. Cargo still runs the real test command,
including doctests. Its exit status and every required case outcome must pass.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import signal
import subprocess
from contextlib import suppress
from datetime import UTC, datetime
from pathlib import Path
from xml.etree import ElementTree

ROOT = Path(__file__).resolve().parents[2]
WORKSPACE = ROOT / "services/hermes-manager"
CATALOGUE = ROOT / "tests/verification/cases.json"
LEVELS = {"unit", "integration", "e2e"}
CARGO_PROFILES = {
    "rust": ["--workspace"],
    "rust-faults": [
        "-p",
        "hermes-manager",
        "--features",
        "fault-injection",
        "--test",
        "process",
    ],
    "rust-browser": ["-p", "hermes-http", "--test", "dashboard_browser"],
    "rust-cluster": [
        "-p",
        "hermes-kube",
        "--test",
        "cluster",
        "--test",
        "admission",
    ],
    "rust-controller": [
        "-p",
        "hermes-manager",
        "--features",
        "fault-injection",
        "--test",
        "controller_process",
    ],
}
IGNORED_PROFILES = {"rust-browser", "rust-cluster", "rust-controller"}
# The default Python profile never needs infrastructure; a test that does
# carries a capability marker and belongs to the profile that provides it.
PYTHON_PROFILES = {
    "python": "not (cluster or database or native or browser or destructive "
    "or multi_node or soak)",
    "python-native": "native",
    "python-cluster": "cluster and not destructive",
    "python-destructive": "destructive",
}
PROFILE_INPUTS = {
    "python-cluster": ("HERMES_TEST_K3D",),
    "python-destructive": ("HERMES_TEST_K3D",),
    "python-native": ("HERMES_TEST_NATIVE_IMAGE",),
    "rust": ("DATABASE_URL",),
    "rust-faults": ("DATABASE_URL",),
    "rust-browser": ("DATABASE_URL", "MANAGER_UI_DIST"),
    "rust-cluster": ("HERMES_TEST_KUBECONFIG",),
    "rust-controller": ("DATABASE_URL", "HERMES_TEST_KUBECONFIG"),
}


def capture(args, *, cwd=ROOT, timeout=60):
    """Bound the whole process group; keep command output private in memory."""
    with subprocess.Popen(
        args,
        cwd=cwd,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        start_new_session=True,
    ) as process:
        try:
            output, _ = process.communicate(timeout=timeout)
        except BaseException:
            with suppress(ProcessLookupError):
                os.killpg(process.pid, signal.SIGKILL)
            process.communicate()
            raise
    return process.returncode, output


def case_level(item):
    levels = {marker.name for marker in item.iter_markers() if marker.name in LEVELS}
    if len(levels) != 1:
        raise ValueError("Each collected test needs exactly one test level")
    return levels.pop()


class PythonCases:
    def __init__(self):
        self.cases = {}

    def pytest_collection_finish(self, session):
        self.cases = {
            item.nodeid: {
                "level": case_level(item),
                "status": "collected",
                "seconds": 0.0,
            }
            for item in session.items
        }

    def pytest_runtest_logreport(self, report):
        case = self.cases[report.nodeid]
        case["seconds"] += report.duration
        if report.failed:
            case["status"] = "failed"
        elif case["status"] != "failed":
            if report.skipped or hasattr(report, "wasxfail"):
                case["status"] = "skipped"
            elif report.when == "call" and case["status"] == "collected":
                case["status"] = "passed"


def python_cases(*, execute, profile="python"):
    import pytest

    recorder = PythonCases()
    arguments = (
        ["-q", "--tb=no", "--show-capture=no", "--no-summary"]
        if execute
        else ["--collect-only", "-q"]
    )
    code = pytest.main([*arguments, "-m", PYTHON_PROFILES[profile]], plugins=[recorder])
    return int(code), recorder.cases


def artifact_key(artifact):
    package = Path(artifact["manifest_path"]).parent.name
    target = artifact["target"]
    return f"{package}/{target['kind'][0]}:{target['name']}"


def cargo_artifacts(output):
    artifacts = {}
    for line in output.splitlines():
        if not line.startswith("{"):
            continue
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (
            row.get("reason") == "compiler-artifact"
            and row["profile"]["test"]
            and row.get("executable")
        ):
            artifacts[Path(row["executable"]).name] = row
    return artifacts


def listed_cases(output):
    return {
        line.removesuffix(": test")
        for line in output.splitlines()
        if line.endswith(": test")
    }


def cargo_collection(profile):
    arguments = [
        "cargo",
        "test",
        *CARGO_PROFILES[profile],
        "--locked",
        "--message-format=json",
    ]
    code, output = capture([*arguments, "--no-run"], cwd=WORKSPACE, timeout=1800)
    if code:
        raise RuntimeError("Cargo collection build failed")
    artifacts = cargo_artifacts(output)
    cases = {}
    excluded = set()
    for artifact in artifacts.values():
        executable = artifact["executable"]
        code, listing = capture(
            [executable, "--list", "--format", "terse"], cwd=WORKSPACE
        )
        ignored_code, ignored = capture(
            [executable, "--list", "--ignored", "--format", "terse"],
            cwd=WORKSPACE,
        )
        if code or ignored_code:
            raise RuntimeError("Cargo test listing failed")
        all_names, ignored_names = listed_cases(listing), listed_cases(ignored)
        selected = (
            ignored_names if profile in IGNORED_PROFILES else all_names - ignored_names
        )
        prefix = artifact_key(artifact)
        excluded.update(f"{prefix}::{name}" for name in all_names - selected)
        for name in selected:
            cases[f"{prefix}::{name}"] = {
                "level": rust_level(prefix, name),
                "status": "collected",
            }
    return arguments, artifacts, cases, excluded


def rust_level(target, name):
    # Classify the exercised boundary, including actual I/O inside library tests.
    package, kind = target.split("/", 1)
    if package == "hermes-store":
        return "integration"
    if package == "hermes-native" and (
        name.startswith("probe::tests::")
        or name.endswith(
            "the_real_initializer_receipt_verifies_for_exactly_its_owner_and_key"
        )
        or name.endswith(
            "a_worker_is_ready_only_with_its_pinned_host_key_and_the_declared_identity"
        )
        or name.endswith("unusable_inputs_and_unreachable_workers_are_not_ready_yet")
    ):
        return "integration"
    if kind.startswith("test:") and package not in {
        "hermes-architecture",
        "hermes-render",
        "hermes-wire",
    }:
        return "integration"
    return "unit"


def cargo_results(output, artifacts):
    results = {}
    prefix = None
    for line in output.splitlines():
        running = re.fullmatch(r"\s*Running .+ \((.+)\)", line)
        if running:
            artifact = artifacts.get(Path(running[1]).name)
            prefix = artifact_key(artifact) if artifact else None
        elif re.match(r"\s*Doc-tests ", line):
            prefix = None
        elif prefix:
            result = re.fullmatch(
                r"test (\S+)(?: - should panic)? \.\.\. (ok|FAILED|ignored)(?:, .*)?",
                line,
            )
            if result:
                identity = f"{prefix}::{result[1]}"
                if identity in results:
                    raise ValueError("Duplicate Cargo execution outcome")
                results[identity] = {
                    "ok": "passed",
                    "FAILED": "failed",
                    "ignored": "skipped",
                }[result[2]]
    return results


def rust_cases(profile, *, execute):
    arguments, artifacts, cases, excluded = cargo_collection(profile)
    if not execute:
        return 0, cases
    print(f"verification: running {len(cases)} collected {profile} cases", flush=True)
    test_arguments = ["--color=never"]
    if profile in IGNORED_PROFILES:
        test_arguments.append("--ignored")
    if profile == "rust-cluster":
        test_arguments.extend(["--test-threads", "2"])
    if profile == "rust-controller":
        # Each case installs the cluster-scoped admission objects for its own
        # control namespace; two at once would overwrite each other's.
        test_arguments.extend(["--test-threads", "1"])
    code, output = capture(
        [*arguments, "--no-fail-fast", "--", *test_arguments],
        cwd=WORKSPACE,
        timeout=2400,
    )
    for identity, status in cargo_results(output, artifacts).items():
        if identity in excluded:
            continue
        if identity not in cases:
            raise ValueError("Uncollected Cargo execution outcome")
        cases[identity]["status"] = status
    return code, cases


def read_catalogue(path=CATALOGUE):
    document = json.loads(path.read_text())
    if document["schemaVersion"] != 1 or set(document["profiles"]) != {
        *PYTHON_PROFILES,
        *CARGO_PROFILES,
    }:
        raise ValueError("Unsupported case catalogue")
    for cases in document["profiles"].values():
        if not cases or any(level not in LEVELS for level in cases.values()):
            raise ValueError("Each profile needs cases with valid levels")
    return document


def gate(expected, actual, *, execute):
    problems = []
    if not actual:
        problems.append("empty_collection")
    problems.extend(
        f"missing:{identity}" for identity in sorted(expected.keys() - actual.keys())
    )
    problems.extend(
        f"unexpected:{identity}" for identity in sorted(actual.keys() - expected.keys())
    )
    for identity in sorted(expected.keys() & actual.keys()):
        if actual[identity]["level"] != expected[identity]:
            problems.append(f"wrong_level:{identity}")
        if execute and actual[identity]["status"] != "passed":
            problems.append(f"not_passed:{identity}")
    return problems


def markdown_table(document, heading):
    section = document.split(f"## {heading}\n", 1)[1].split("\n## ", 1)[0]
    lines = iter(section.splitlines())
    for line in lines:
        if line.startswith("|"):
            break
    next(lines)  # separator
    rows = []
    for line in lines:
        if not line.startswith("|"):
            break
        rows.append([cell.strip().strip("`") for cell in line.strip("|").split(" | ")])
    return rows


def obligations(catalogue, document):
    """Resolve the existing cited examples to exact IDs without counting helpers."""
    names = {}
    for profile, cases in catalogue["profiles"].items():
        for identity in cases:
            name = identity.rsplit("::", 1)[-1].split("[", 1)[0]
            names.setdefault(name, []).append({"profile": profile, "id": identity})
    rows = []
    sections = [
        ("Requirements", 2),
        ("Acceptance", 1),
        ("Interfaces", 3),
        ("Operational bounds", 1),
    ]
    for heading, column in sections:
        for row in markdown_table(document, heading):
            identity = f"{row[1]}#{row[2]}" if heading == "Interfaces" else row[0]
            cited = [name.strip().strip("`") for name in row[column].split(",")]
            trace = {
                "section": heading,
                "id": identity,
                "declaredStatus": row[-1],
                "cataloguedCases": [
                    case for name in cited for case in names.get(name, [])
                ],
                "uncollected": [
                    name for name in cited if name not in names and name != "—"
                ],
            }
            if heading == "Interfaces":
                method, path = row[0].split(" ", 1)
                trace.update(
                    contract=row[1], operationId=row[2], method=method, path=path
                )
            rows.append(trace)
    return rows


def source_state():
    code, revision = capture(["git", "rev-parse", "HEAD"])
    status_code, status = capture(["git", "status", "--porcelain"])
    if code or status_code:
        raise RuntimeError("Source identification failed")
    return {"revision": revision.strip(), "dirty": bool(status.strip())}


def write_private(path, content):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    descriptor = os.open(
        path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW, 0o600
    )
    with os.fdopen(descriptor, "w") as output:
        os.fchmod(output.fileno(), 0o600)
        output.write(content)


def write_report(path, report):
    write_private(path, json.dumps(report, indent=2, sort_keys=True) + "\n")


def safe_results(expected, actual, problems):
    """Only reviewed identities may appear verbatim in shareable artifacts."""
    replacements = {
        identity: "unreviewed:" + hashlib.sha256(identity.encode()).hexdigest()
        for identity in actual.keys() - expected.keys()
    }
    cases = {
        replacements.get(identity, identity): case for identity, case in actual.items()
    }
    safe_problems = [
        "unexpected:" + replacements[problem.removeprefix("unexpected:")]
        if problem.startswith("unexpected:")
        else problem
        for problem in problems
    ]
    return cases, safe_problems


def write_junit(path, report):
    suite = ElementTree.Element("testsuite", name=report["profile"])
    failures = 0
    for identity, case in sorted(report["cases"].items()):
        test = ElementTree.SubElement(
            suite,
            "testcase",
            classname=report["profile"],
            name=identity,
        )
        if "seconds" in case:
            test.set("time", str(round(case["seconds"], 6)))
        if case["status"] == "skipped":
            ElementTree.SubElement(test, "skipped", message="Required case skipped")
        elif case["status"] != "passed":
            ElementTree.SubElement(
                test, "failure", message="Required case did not pass"
            )
            failures += 1
    if report["problems"]:
        test = ElementTree.SubElement(suite, "testcase", name="required_case_gate")
        ElementTree.SubElement(
            test, "failure", message="Required collection/execution gate failed"
        )
        failures += 1
    suite.set("tests", str(len(suite)))
    suite.set("failures", str(failures))
    suite.set(
        "skipped",
        str(sum(case["status"] == "skipped" for case in report["cases"].values())),
    )
    write_private(path, ElementTree.tostring(suite, encoding="unicode") + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["inventory", "run"])
    parser.add_argument("profile", choices=[*PYTHON_PROFILES, *CARGO_PROFILES])
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    started = datetime.now(UTC).isoformat()
    source = source_state()
    catalogue = read_catalogue()
    execute = args.action == "run"
    print(f"verification: {args.profile} {args.action}", flush=True)
    cases = {}
    problems = [
        f"missing_input:{name}"
        for name in PROFILE_INPUTS.get(args.profile, ())
        if execute and not os.environ.get(name)
    ]
    try:
        if not problems:
            if args.profile in PYTHON_PROFILES:
                code, cases = python_cases(execute=execute, profile=args.profile)
            else:
                code, cases = rust_cases(args.profile, execute=execute)
            problems = gate(catalogue["profiles"][args.profile], cases, execute=execute)
            if code:
                problems.append("runner_failed")
    except OSError, RuntimeError, ValueError, subprocess.TimeoutExpired:
        cases, problems = {}, ["runner_setup_or_execution_failed"]
    cases, problems = safe_results(catalogue["profiles"][args.profile], cases, problems)
    report = {
        "schemaVersion": 1,
        "profile": args.profile,
        "action": args.action,
        "source": source,
        "catalogueDigest": hashlib.sha256(CATALOGUE.read_bytes()).hexdigest(),
        "startedAt": started,
        "finishedAt": datetime.now(UTC).isoformat(),
        "cases": cases,
        "problems": problems,
        "obligations": obligations(catalogue, (WORKSPACE / "TESTS.md").read_text()),
        "scope": (
            "Cited examples only; uncollected helpers and missing variants "
            "remain open gaps."
        ),
    }
    write_report(args.output, report)
    if execute:
        write_junit(args.output.with_suffix(".xml"), report)
    print(f"verification: {len(cases)} cases, {len(problems)} problems", flush=True)
    for problem in problems:
        print(problem)
    return int(bool(problems))


if __name__ == "__main__":
    raise SystemExit(main())
