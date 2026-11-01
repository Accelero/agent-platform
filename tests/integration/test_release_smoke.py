"""The manager image's release payload must also run in the default suite."""

import runpy

import pytest

from agent_platform.build import ROOT

pytestmark = pytest.mark.integration


def test_the_runtime_manager_release_smoke_payload_uses_the_current_package():
    """R4, R10: prevent release validation from importing a removed entrypoint."""
    runpy.run_path(str(ROOT / "tests/runtime_smoke.py"), run_name="__main__")
