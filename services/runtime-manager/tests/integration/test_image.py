"""The runtime-manager release image serializes the worker handoff (R4)."""

import pytest

from agent_platform.build import ROOT
from tests.support.images import run_in_image

pytestmark = pytest.mark.image
PAYLOAD = ROOT / "services/runtime-manager/tests/handoff_payload.py"


@pytest.mark.artifact
def test_the_manager_image_serializes_the_worker_handoff(platform_image):
    run_in_image(platform_image("runtime-manager"), "python", "-", payload=PAYLOAD)
