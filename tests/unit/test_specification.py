"""Keep requirements, proving tests and package dependencies in agreement."""

import ast
import importlib.util
import re
from pathlib import Path

import pytest

from agent_platform.build import ROOT

pytestmark = pytest.mark.unit
PACKAGE = ROOT / "services/runtime-manager/runtime_manager"
REQUIREMENT = re.compile(r"\b[XR]\d+\b")


def test_every_requirement_names_existing_tests_and_test_ids_are_documented():
    spec = (ROOT / "docs/runtime-manager/specification.md").read_text()
    rows = {}
    for line in spec.splitlines():
        if re.match(r"\| [XR]\d+ \|", line):
            fields = [part.strip() for part in line.strip("|").split("|")]
            assert fields[0] not in rows, f"duplicate requirement {fields[0]}"
            rows[fields[0]] = re.findall(r"`(test_\w+)`", fields[-1])
    tests, mentioned = set(), set()
    for path in (ROOT / "tests").rglob("test_*.py"):
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


def imports(path: Path) -> set[str]:
    parts = path.relative_to(PACKAGE.parent).with_suffix("").parts
    package = ".".join(parts[:-1])
    names = set()
    for node in ast.walk(ast.parse(path.read_text())):
        if isinstance(node, ast.Import):
            names.update(alias.name for alias in node.names)
        elif isinstance(node, ast.ImportFrom):
            base = (
                importlib.util.resolve_name(
                    "." * node.level + (node.module or ""), package
                )
                if node.level
                else node.module
            )
            names.add(base)
            names.update(base + "." + alias.name for alias in node.names)
    return names


def test_domain_and_adapters_keep_dependencies_pointing_inward():
    for path in (PACKAGE / "domain").rglob("*.py"):
        assert all(
            name == "runtime_manager.domain"
            or name.startswith("runtime_manager.domain.")
            for name in imports(path)
            if name.startswith("runtime_manager")
        )
    forbidden = {
        "service.py": ("api", "server"),
        "api.py": ("server",),
        "cluster.py": ("service",),
        "store.py": ("service",),
    }
    for filename, modules in forbidden.items():
        found = imports(PACKAGE / filename)
        for module in modules:
            prefix = "runtime_manager." + module
            assert not any(
                name == prefix or name.startswith(prefix + ".") for name in found
            ), filename
