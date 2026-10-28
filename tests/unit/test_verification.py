"""The gate fails when collected or executed evidence disappears."""

import json
from pathlib import Path
from types import SimpleNamespace

import pytest

from tests.support import verification as verify

pytestmark = pytest.mark.unit


@pytest.mark.parametrize("status", ["collected", "failed", "skipped"])
def test_a_required_case_must_actually_pass(status):
    assert verify.gate(
        {"a_case": "unit"},
        {"a_case": {"level": "unit", "status": status}},
        execute=True,
    ) == ["not_passed:a_case"]


def test_an_empty_or_incomplete_collection_cannot_satisfy_the_gate():
    expected = {"memory::same_key": "unit", "postgres::same_key": "integration"}
    assert verify.gate(expected, {}, execute=False) == [
        "empty_collection",
        "missing:memory::same_key",
        "missing:postgres::same_key",
    ]
    assert verify.gate(
        expected, {"memory::same_key": {"level": "unit"}}, execute=False
    ) == [
        "missing:postgres::same_key",
    ]


def test_new_cases_and_changed_proving_levels_need_ledger_review():
    assert verify.gate(
        {"known": "integration"},
        {"known": {"level": "unit"}, "new": {"level": "unit"}},
        execute=False,
    ) == ["unexpected:new", "wrong_level:known"]


def test_collection_evidence_does_not_require_an_execution_outcome():
    assert (
        verify.gate(
            {"a_case": "unit"},
            {"a_case": {"level": "unit", "status": "collected"}},
            execute=False,
        )
        == []
    )


def artifact(package, target="gateway"):
    return {
        "reason": "compiler-artifact",
        "profile": {"test": True},
        "manifest_path": f"/workspace/crates/{package}/Cargo.toml",
        "target": {"kind": ["test"], "name": target},
        "executable": f"/workspace/target/debug/deps/{target}-{package}",
    }


def test_equal_test_names_in_different_cargo_targets_are_independent():
    first, second = artifact("hermes-proxy"), artifact("hermes-gateway")
    output = "\n".join(json.dumps(row) for row in [first, second])
    output += "\n" + "\n".join(
        [
            "     Running tests/gateway.rs (target/debug/deps/gateway-hermes-proxy)",
            "test same_name ... ok",
            "     Running tests/gateway.rs (target/debug/deps/gateway-hermes-gateway)",
            "test same_name ... FAILED",
        ]
    )
    assert verify.cargo_results(output, verify.cargo_artifacts(output)) == {
        "hermes-proxy/test:gateway::same_name": "passed",
        "hermes-gateway/test:gateway::same_name": "failed",
    }


def test_ignored_results_and_doctests_do_not_turn_into_passes():
    row = artifact("hermes-http", "dashboard_browser")
    results = verify.cargo_results(
        "\n".join(
            [
                f"     Running tests/dashboard_browser.rs ({row['executable']})",
                "test production_dashboard_browser_csp ... ignored, needs a browser",
                "   Doc-tests hermes_http",
                "test untracked_doc_example ... ok",
            ]
        ),
        {Path(row["executable"]).name: row},
    )
    identity = "hermes-http/test:dashboard_browser::production_dashboard_browser_csp"
    assert results == {identity: "skipped"}


def test_duplicate_cargo_results_are_rejected():
    row = artifact("hermes-http")
    output = (
        f"Running tests/gateway.rs ({row['executable']})\n"
        "test one ... ok\ntest one ... ok"
    )
    with pytest.raises(ValueError, match="Duplicate"):
        verify.cargo_results(output, {Path(row["executable"]).name: row})


def test_expected_panics_are_counted_as_executed_tests():
    row = artifact("hermes-architecture", "traceability")
    output = (
        f"Running tests/traceability.rs ({row['executable']})\n"
        "test rejects_duplicates - should panic ... ok"
    )
    assert verify.cargo_results(output, {Path(row["executable"]).name: row}) == {
        "hermes-architecture/test:traceability::rejects_duplicates": "passed"
    }


@pytest.mark.parametrize(
    "levels", [[], ["unit", "integration"]], ids=["missing", "ambiguous"]
)
def test_missing_and_ambiguous_levels_are_configuration_errors(levels):
    item = SimpleNamespace(
        iter_markers=lambda: [SimpleNamespace(name=name) for name in levels]
    )
    with pytest.raises(ValueError, match="exactly one"):
        verify.case_level(item)


def test_a_helper_name_is_not_execution_evidence():
    catalogue = {
        "profiles": {"python": {"tests/test_one.py::test_real[denied]": "unit"}}
    }
    document = "\n".join(
        f"## {heading}\n\n| ID | Tests | Status |\n| --- | --- | --- |\n{rows}\n"
        for heading, rows in [
            (
                "Requirements",
                "| R1 | description | `test_real`, `merely_a_helper` | partial |",
            ),
            ("Acceptance", "| Isolated | `test_real` | partial |"),
            (
                "Interfaces",
                "| POST /auth/logout | openapi.yaml | adminLogout "
                "| `test_real` | partial |",
            ),
            ("Operational bounds", "| Freshness | `merely_a_helper` | partial |"),
        ]
    )
    rows = verify.obligations(catalogue, document)
    assert rows[0]["cataloguedCases"] == [
        {"profile": "python", "id": "tests/test_one.py::test_real[denied]"}
    ]
    assert rows[0]["uncollected"] == ["merely_a_helper"]
    assert rows[2]["id"] == "openapi.yaml#adminLogout"
    assert rows[2]["method"] == "POST"
    assert rows[2]["path"] == "/auth/logout"
    assert rows[2]["contract"] == "openapi.yaml"
    assert rows[2]["operationId"] == "adminLogout"
    assert rows[3]["cataloguedCases"] == []


def test_reviewed_ledger_accounts_for_every_cited_helper():
    catalogue = verify.read_catalogue()
    rows = verify.obligations(catalogue, (verify.WORKSPACE / "TESTS.md").read_text())
    unresolved = {name for row in rows for name in row["uncollected"]}
    assert unresolved == set(catalogue["uncollected"])
    for exception in catalogue["uncollected"].values():
        assert exception["gap"] in {"G01", "G03"}
        assert (verify.ROOT / exception["source"]).is_file()
    assert {row["section"] for row in rows} == {
        "Requirements",
        "Acceptance",
        "Interfaces",
        "Operational bounds",
    }
    assert len([row for row in rows if row["section"] == "Interfaces"]) == 58
    assert len([row for row in rows if row["section"] == "Operational bounds"]) == 9


def test_machine_reports_do_not_capture_failure_text_or_environment(tmp_path):
    recorder = verify.PythonCases()
    recorder.cases = {
        "test_denied": {"level": "unit", "status": "collected", "seconds": 0.0}
    }
    canary = "generated-canary-secret"
    recorder.pytest_runtest_logreport(
        SimpleNamespace(
            nodeid="test_denied",
            duration=0.1,
            failed=True,
            skipped=False,
            when="call",
            longrepr=canary,
            capstdout=canary,
            capstderr=canary,
        )
    )
    path = tmp_path / "private/report.json"
    verify.write_report(path, recorder.cases)
    assert canary not in path.read_text()
    assert json.loads(path.read_text())["test_denied"]["status"] == "failed"
    assert path.stat().st_mode & 0o077 == 0


def test_reports_do_not_follow_an_existing_symlink(tmp_path):
    target = tmp_path / "foreign"
    target.write_text("keep")
    report = tmp_path / "report.json"
    report.symlink_to(target)
    with pytest.raises(OSError):
        verify.write_report(report, {})
    assert target.read_text() == "keep"


def test_unreviewed_parameter_values_are_not_written_to_reports(tmp_path):
    identity = "test_invalid[generated-canary-secret]"
    cases, problems = verify.safe_results(
        {},
        {identity: {"level": "unit", "status": "failed"}},
        [f"unexpected:{identity}"],
    )
    report = {"profile": "python", "cases": cases, "problems": problems}
    verify.write_report(tmp_path / "report.json", report)
    verify.write_junit(tmp_path / "report.xml", report)
    assert "generated-canary-secret" not in (tmp_path / "report.json").read_text()
    assert "generated-canary-secret" not in (tmp_path / "report.xml").read_text()
    assert list(cases)[0].startswith("unreviewed:")
