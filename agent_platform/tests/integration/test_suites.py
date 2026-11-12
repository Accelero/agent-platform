"""The suite plugin in real pytest sessions: levels, selection, shards and CI."""

import pytest

from tests.support import suites

pytest_plugins = ("pytester",)


@pytest.fixture
def layout(pytester, monkeypatch):
    """A repository with one component's unit and integration suites and e2e."""
    registered = (
        suites.Suite("demo-unit", "demo/tests/unit"),
        suites.Suite("demo-integration", "demo/tests/integration"),
        suites.Suite("e2e", "tests/e2e"),
    )
    monkeypatch.setattr(suites, "ROOT", pytester.path)
    monkeypatch.setattr(suites, "SUITES", registered)
    monkeypatch.setattr(suites, "BY_NAME", {s.name: s for s in registered})
    monkeypatch.delenv("CI", raising=False)
    pytester.makepyfile(
        **{
            "demo/tests/unit/test_pure.py": """
                import pytest
                def test_one(request):
                    assert request.node.get_closest_marker("unit")
                def test_two(): pass
                def test_three(): pass
                def test_four(): pass
            """,
            "demo/tests/integration/test_process.py": """
                import subprocess, sys
                def test_runs_a_process():
                    subprocess.run([sys.executable, "-c", "pass"], check=True)
            """,
            "tests/e2e/test_flow.py": """
                import pytest
                def test_flow(): pass
                @pytest.mark.manual
                def test_needs_credentials(): pass
            """,
        }
    )
    return pytester


def run(pytester, *args):
    return pytester.runpytest_inprocess(
        "-p", "tests.support.suites", "--import-mode=importlib", *args
    )


def test_levels_come_from_directories_and_manual_tests_are_opt_in(layout):
    run(layout, "-m", "unit").assert_outcomes(passed=4, deselected=3)
    run(layout).assert_outcomes(passed=6, deselected=1)
    run(layout, "--manual", "-m", "e2e").assert_outcomes(passed=2, deselected=5)


def test_a_suite_and_its_shards_cover_each_test_once(layout):
    run(layout, "--suite", "demo-integration").assert_outcomes(passed=1, deselected=6)
    names = []
    for index in (1, 2, 3):
        result = run(layout, "--suite", "demo-unit", "--shard", f"{index}/3", "-v")
        names += [line.split()[0] for line in result.outlines if "PASSED" in line]
    assert sorted(names) == sorted(
        f"demo/tests/unit/test_pure.py::test_{n}"
        for n in ("one", "two", "three", "four")
    )


def test_a_unit_test_cannot_start_a_process_or_open_a_socket(layout):
    layout.makepyfile(
        **{
            "demo/tests/unit/test_leaks.py": """
                import socket, subprocess, sys
                def test_process():
                    subprocess.run([sys.executable, "-c", "pass"])
                def test_socket():
                    socket.create_connection(("127.0.0.1", 9), timeout=1)
            """
        }
    )
    result = run(layout, "--suite", "demo-unit")
    result.assert_outcomes(passed=4, failed=2, deselected=3)
    result.stdout.fnmatch_lines(["*move this test to integration/*"])


def test_misplaced_tests_and_handwritten_levels_are_rejected(layout):
    layout.makepyfile(**{"elsewhere/test_loose.py": "def test_loose(): pass"})
    result = run(layout, "elsewhere")
    assert result.ret == pytest.ExitCode.USAGE_ERROR
    layout.makepyfile(
        **{
            "demo/tests/unit/test_marked.py": """
                import pytest
                pytestmark = pytest.mark.integration
                def test_marked(): pass
            """
        }
    )
    result = run(layout, "demo")
    assert result.ret == pytest.ExitCode.USAGE_ERROR
    result.stderr.fnmatch_lines(["*its directory sets the level*"])


def test_check_suites_reports_empty_suites(layout):
    (layout.path / "tests/e2e/test_flow.py").unlink()
    result = run(layout, "--check-suites", "--collect-only")
    assert result.ret == pytest.ExitCode.USAGE_ERROR
    result.stderr.fnmatch_lines(["*Suites without tests: e2e*"])


def test_a_skip_fails_in_ci(layout, monkeypatch):
    layout.makepyfile(
        **{
            "demo/tests/unit/test_skips.py": """
                import pytest
                def test_skipped():
                    pytest.skip("no tool here")
            """
        }
    )
    run(layout, "--suite", "demo-unit").assert_outcomes(
        passed=4, skipped=1, deselected=3
    )
    monkeypatch.setenv("CI", "true")
    result = run(layout, "--suite", "demo-unit")
    result.assert_outcomes(passed=4, failed=1, deselected=3)
    result.stdout.fnmatch_lines(["*Skipped in CI*no tool here*"])
