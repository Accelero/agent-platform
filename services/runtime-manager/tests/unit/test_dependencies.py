"""Dependencies of the manager package point inward (specification, architecture)."""

import ast
import importlib.util
from pathlib import Path

from agent_platform.build import ROOT

PACKAGE = ROOT / "services/runtime-manager/runtime_manager"


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
