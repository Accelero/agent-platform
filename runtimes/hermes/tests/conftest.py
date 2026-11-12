"""Fixtures for the Hermes runtime suites."""

import importlib.util

import pytest

from agent_platform.build import ROOT


@pytest.fixture
def initializer(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.syspath_prepend(str(ROOT / "runtimes/hermes"))
    spec = importlib.util.spec_from_file_location(
        "runtime_init", ROOT / "runtimes/hermes/runtime-init.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module
