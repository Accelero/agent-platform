"""The tools release image carries its administrator tools and no host keys."""

import pytest

from tests.support.images import run_in_image

pytestmark = [pytest.mark.image, pytest.mark.artifact]


def test_the_tools_image_runs_its_python_libraries_and_clients(platform_image):
    output = run_in_image(
        platform_image("tools"),
        "python",
        "-c",
        "import boto3, kubernetes, yaml, platform_bootstrap.__main__, "
        "platform_recovery.__main__; import subprocess; "
        'subprocess.run(["pg_dump", "--version"], check=True); '
        'subprocess.run(["restic", "version"], check=True)',
    )
    assert "pg_dump (PostgreSQL) 17" in output
    assert "restic" in output


def test_the_tools_image_ships_no_host_keys(platform_image):
    run_in_image(
        platform_image("tools"),
        "python",
        "-c",
        "from pathlib import Path; "
        "assert not list(Path('/etc/ssh').glob('ssh_host_*_key')); "
        "assert not Path('/etc/ssl/private/ssl-cert-snakeoil.key').exists()",
        options=("--read-only", "--user", "0:0"),
    )
