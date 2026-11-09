"""Require an explicit proving level for every collected test.

A test is `unit`, `integration` or `e2e` (see pyproject.toml). The closest marker
wins, so a test may override its module's level. `image` tests build and run real
container images and carry a level as well. Every run includes them. A missing
container prerequisite skips them locally but fails them in CI, where `CI` is set.
"""

import shutil
import subprocess
from pathlib import Path

import pytest

LEVELS = ("unit", "integration", "e2e")


def pytest_collection_modifyitems(items):
    for item in items:
        closest = None
        levels = []
        for node, marker in item.iter_markers_with_node():
            if marker.name in LEVELS and (closest is None or node is closest):
                closest = node
                levels.append(marker.name)
        if len(levels) != 1:
            raise pytest.UsageError(
                f"{item.nodeid}: mark it with exactly one of {', '.join(LEVELS)}"
            )


@pytest.fixture(scope="session")
def tls(tmp_path_factory) -> tuple[Path, Path]:
    """One self-signed certificate for localhost, shared by the whole session."""
    openssl = shutil.which("openssl")
    if openssl is None:
        pytest.skip("openssl is required for TLS tests")
    directory = tmp_path_factory.mktemp("tls")
    certificate, key = directory / "tls.crt", directory / "tls.key"
    subprocess.run(
        [
            openssl,
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-keyout",
            str(key),
            "-out",
            str(certificate),
            "-subj",
            "/CN=localhost",
            "-addext",
            "subjectAltName=DNS:localhost,IP:127.0.0.1",
        ],
        check=True,
        capture_output=True,
        timeout=30,
    )
    return certificate, key


@pytest.fixture
def world(tmp_path):
    """The execution system with a fake clock and namespace (see
    tests/support/world.py).
    """
    from tests.support.world import make_world

    system = make_world(tmp_path)
    try:
        yield system
    finally:
        system.close()
