"""Prerequisites shared by tests that build and run real container images."""

import os

import pytest


def require_image_prerequisite(available: bool, reason: str) -> None:
    """Skip an image test without Docker or a tool locally; never skip it in CI."""
    if available:
        return
    if os.environ.get("CI"):
        pytest.fail(reason + "; image tests must run in CI")
    pytest.skip(reason)
