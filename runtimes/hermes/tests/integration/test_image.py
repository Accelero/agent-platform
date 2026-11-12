"""The Hermes release image loads the managed execution plugin natively."""

import pytest

from agent_platform.build import ROOT
from tests.support.images import run_in_image

pytestmark = pytest.mark.image
PAYLOAD = ROOT / "runtimes/hermes/tests/execution_payload.py"


@pytest.mark.artifact
def test_the_hermes_image_registers_managed_execution(platform_image):
    run_in_image(
        platform_image("hermes"),
        "/opt/hermes/.venv/bin/python",
        "-",
        payload=PAYLOAD,
    )
