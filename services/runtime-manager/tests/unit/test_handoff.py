"""The serialized worker handoff payload passes against the current package (R4)."""

import runpy

from agent_platform.build import ROOT

PAYLOAD = ROOT / "services/runtime-manager/tests/handoff_payload.py"


def test_the_handoff_payload_passes_with_the_current_package():
    """Prevent the image check from importing a removed entrypoint."""
    runpy.run_path(str(PAYLOAD), run_name="__main__")
