"""Keep requirement tables and their proving tests in agreement."""

import ast
import re

from agent_platform.build import ROOT
from tests.support.suites import SUITES

REQUIREMENT = re.compile(r"\b[XR]\d+\b")


def suite_modules():
    """Every module in a registered suite, so moved tests stay traceable."""
    for suite in SUITES:
        yield from (ROOT / suite.path).rglob("test_*.py")


def test_every_requirement_names_existing_tests_and_test_ids_are_documented():
    spec = (ROOT / "docs/runtime-manager/specification.md").read_text()
    rows = {}
    for line in spec.splitlines():
        if re.match(r"\| [XR]\d+ \|", line):
            fields = [part.strip() for part in line.strip("|").split("|")]
            assert fields[0] not in rows, f"duplicate requirement {fields[0]}"
            rows[fields[0]] = re.findall(r"`(test_\w+)`", fields[-1])
    tests, mentioned = set(), set()
    for path in suite_modules():
        source = path.read_text()
        tests.update(
            node.name
            for node in ast.walk(ast.parse(source))
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
            and node.name.startswith("test_")
        )
        mentioned.update(REQUIREMENT.findall(source))
    assert rows
    assert not mentioned - rows.keys(), (
        f"undocumented requirements: {mentioned - rows.keys()}"
    )
    for requirement, names in rows.items():
        assert names, f"{requirement} has no proving test"
        assert not set(names) - tests, (
            f"{requirement}: unknown tests {set(names) - tests}"
        )


def test_hindsight_requirements_name_existing_tests():
    specification = (ROOT / "docs/hindsight/specification.md").read_text()
    rows = {}
    for line in specification.splitlines():
        if re.match(r"\| H\d+ \|", line):
            fields = [part.strip() for part in line.strip("|").split("|")]
            assert fields[0] not in rows, f"duplicate requirement {fields[0]}"
            rows[fields[0]] = re.findall(r"`(test_\w+)`", fields[-1])
    tests, mentioned = set(), set()
    for path in suite_modules():
        source = path.read_text()
        tests.update(
            node.name
            for node in ast.walk(ast.parse(source))
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
            and node.name.startswith("test_")
        )
        mentioned.update(re.findall(r"\bH\d+\b", source))
    assert set(rows) == {f"H{number}" for number in range(1, 15)}
    assert not mentioned - rows.keys(), (
        f"undocumented requirements: {mentioned - rows.keys()}"
    )
    for requirement, names in rows.items():
        assert names, f"{requirement} has no proving test"
        assert not set(names) - tests, (
            f"{requirement}: unknown tests {set(names) - tests}"
        )
