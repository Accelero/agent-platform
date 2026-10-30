"""Third-party imports of the host-side tests and tooling are declared dependencies."""

import ast
import re
import sys
import tomllib
from importlib import metadata
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
LOCAL = {"tests", "agent_platform"}
# These scripts run inside the Hermes image against its own packages.
IMAGE_SIDE = {
    ROOT / "tests/hermes_execution_smoke.py",
    ROOT / "tests/personal_hermes/native_owner_payload.py",
}
SOURCES = sorted(
    path
    for directory in ("tests", "agent_platform")
    for path in (ROOT / directory).rglob("*.py")
    if path not in IMAGE_SIDE and ".venv" not in path.parts
)


def normalize(name):
    return re.sub(r"[-_.]+", "-", name).lower()


def declared():
    project = tomllib.loads((ROOT / "pyproject.toml").read_text())
    requirements = project["project"]["dependencies"]
    for group in project["dependency-groups"].values():
        requirements += [entry for entry in group if isinstance(entry, str)]
    return {
        normalize(re.split(r"[=<>!~\[ ;]", entry, maxsplit=1)[0])
        for entry in requirements
    }


def imported(path):
    names = set()
    for node in ast.walk(ast.parse(path.read_text())):
        if isinstance(node, ast.Import):
            names.update(alias.name.split(".")[0] for alias in node.names)
        elif isinstance(node, ast.ImportFrom) and node.level == 0 and node.module:
            names.add(node.module.split(".")[0])
    return names - sys.stdlib_module_names - LOCAL - {"__future__"}


def undeclared(path):
    owners = metadata.packages_distributions()
    direct = declared()
    return {
        name
        for name in imported(path)
        if not {normalize(owner) for owner in owners.get(name, [name])} & direct
    }


@pytest.mark.unit
@pytest.mark.parametrize(
    "path", SOURCES, ids=[str(path.relative_to(ROOT)) for path in SOURCES]
)
def test_third_party_imports_are_declared_direct_dependencies(path):
    assert not undeclared(path), f"{path.relative_to(ROOT)} imports undeclared modules"


@pytest.mark.unit
def test_the_checker_rejects_a_transitive_only_import(tmp_path):
    source = tmp_path / "uses_transitive.py"
    source.write_text("import attrs\nimport yaml\nfrom os import path\n")
    assert undeclared(source) == {"attrs"}
