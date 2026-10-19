"""Test the owner hook against the exact public Hermes base used by the image."""

import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HERMES = (
    "nousresearch/hermes-agent@sha256:"
    "d4da4a40cd7a28aba983775d9fd31d94cbf153eeb0cb9e844d6d0f612b7c24db"
)


def main():
    subprocess.run(
        [
            "docker",
            "run",
            "--rm",
            "--network",
            "none",
            "--read-only",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--user",
            "1000:1000",
            "--tmpfs",
            "/tmp:mode=1777",
            "--env",
            "PYTHONDONTWRITEBYTECODE=1",
            "--env",
            "HERMES_HOME=/tmp/home",
            "--mount",
            f"type=bind,src={ROOT / 'runtimes/hermes'},dst=/platform,readonly",
            "--mount",
            f"type=bind,src={ROOT / 'tests/personal_hermes'},dst=/tests,readonly",
            "--entrypoint",
            "/opt/hermes/.venv/bin/python",
            HERMES,
            "/tests/native_owner_payload.py",
        ],
        check=True,
        timeout=180,
    )


if __name__ == "__main__":
    main()
