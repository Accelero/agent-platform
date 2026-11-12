"""The suite registry matches the test layout, pytest's paths and the CI matrix."""

import tomllib

import pytest

from agent_platform.build import ROOT
from tests.support import suites


def test_every_suite_names_its_component_and_level_and_exists():
    for suite in suites.SUITES:
        assert (ROOT / suite.path).is_dir(), suite.path
        assert suite.level in suites.LEVELS
        if suite.level == "e2e":
            assert (suite.name, suite.path) == ("e2e", "tests/e2e")
        else:
            component, level = suite.name.rsplit("-", 1)
            assert level == suite.level and component
            assert suite.path.endswith(f"/tests/{level}")


def test_pytest_collects_exactly_the_suite_directories():
    configured = tomllib.loads((ROOT / "pyproject.toml").read_text())
    testpaths = configured["tool"]["pytest"]["ini_options"]["testpaths"]
    roots = {suite.path.rsplit("/", 1)[0] for suite in suites.SUITES}
    assert set(testpaths) == (roots - {"tests"}) | {"tests/e2e"}
    for path in testpaths:
        for found in (ROOT / path).rglob("test_*.py"):
            assert suites.suite_of(found) is not None, found


def test_a_selection_expands_into_named_runners_and_shards(monkeypatch):
    monkeypatch.setattr(
        suites,
        "BY_NAME",
        {**suites.BY_NAME, "e2e": suites.Suite("e2e", "tests/e2e", shards=2)},
    )
    selected = suites.select("manager-unit, e2e")
    include = suites.matrix(selected)["include"]
    assert [row["name"] for row in include] == [
        "manager-unit",
        "e2e (1/2)",
        "e2e (2/2)",
    ]
    assert [row["shard"] for row in include] == ["1/1", "1/2", "2/2"]
    assert len(suites.matrix(suites.select("all"))["include"]) >= len(suites.SUITES)


@pytest.mark.parametrize("value", ["", "manager", "manager-unit,unknown"])
def test_unknown_selections_are_refused(value):
    with pytest.raises(ValueError, match="Unknown suite"):
        suites.select(value)


@pytest.mark.parametrize("value", ["0/2", "3/2", "1", "a/b"])
def test_invalid_shards_are_refused(value):
    with pytest.raises(Exception, match="I/N|1 <= I <= N"):
        suites.shard(value)
