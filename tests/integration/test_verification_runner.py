"""Exercise actual pytest phases so setup/teardown failures cannot pass a gate."""

import json
import os
import subprocess
import sys

import pytest

from tests.support.verification import ROOT, PythonCases, gate

pytestmark = pytest.mark.integration


def test_actual_collection_and_all_execution_phases_are_recorded(pytester):
    pytester.makeini("[pytest]\nmarkers = unit: isolated logic\n")
    pytester.makepyfile("""
        import pytest
        pytestmark = pytest.mark.unit

        @pytest.fixture
        def bad_setup():
            raise RuntimeError("generated-secret-setup")

        @pytest.fixture
        def bad_teardown():
            yield
            raise RuntimeError("generated-secret-teardown")

        @pytest.mark.parametrize(
            "actor", ["allowed", "denied"], ids=["allowed", "denied"]
        )
        def test_parameter(actor):
            assert actor in {"allowed", "denied"}

        def test_setup(bad_setup):
            pass

        def test_teardown(bad_teardown):
            pass

        @pytest.mark.skip(reason="unavailable dependency")
        def test_skip():
            pass

        @pytest.mark.xfail
        def test_xfail():
            assert False

        @pytest.mark.xfail
        def test_xpass():
            pass
    """)
    recorder = PythonCases()
    result = pytester.runpytest_inprocess("-q", plugins=[recorder])
    assert result.ret != 0
    statuses = {
        name.split("::")[-1]: case["status"] for name, case in recorder.cases.items()
    }
    assert statuses == {
        "test_parameter[allowed]": "passed",
        "test_parameter[denied]": "passed",
        "test_setup": "failed",
        "test_teardown": "failed",
        "test_skip": "skipped",
        "test_xfail": "skipped",
        "test_xpass": "skipped",
    }
    expected = dict.fromkeys(recorder.cases, "unit")
    assert len(gate(expected, recorder.cases, execute=True)) == 5


def test_a_requested_database_profile_fails_before_running_without_its_fixture(
    tmp_path,
):
    environment = dict(os.environ)
    environment.pop("DATABASE_URL", None)
    output = tmp_path / "missing-database.json"
    result = subprocess.run(
        [
            sys.executable,
            "-m",
            "tests.support.verification",
            "run",
            "rust-faults",
            "--output",
            str(output),
        ],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 1
    report = json.loads(output.read_text())
    assert report["problems"] == ["missing_input:DATABASE_URL"]
    assert report["cases"] == {}
    assert output.with_suffix(".xml").is_file()
