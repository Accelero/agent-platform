"""Fixtures shared by every suite; the root conftest.py loads this plugin."""

import shutil
import subprocess
from pathlib import Path

import pytest

from tests.support.images import platform_image  # noqa: F401  (a fixture)


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
