"""Build the unreleased manager/UI image from its finite reviewed context."""

from __future__ import annotations

import argparse
import subprocess
import tempfile
from pathlib import Path

from agent_platform.command import run
from agent_platform.privacy import ROOT, context_files, policy


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", default="hermes-manager:local")
    args = parser.parse_args()
    definition = policy(ROOT)["development_contexts"]["hermes-manager"]
    files = context_files(ROOT, definition)
    revision = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=ROOT,
        capture_output=True,
        check=True,
        text=True,
        timeout=10,
    ).stdout.strip()
    with tempfile.TemporaryDirectory(prefix="hermes-manager-build-") as temporary:
        context = Path(temporary)
        for name, content in files.items():
            path = context / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
        run(
            [
                "docker",
                "build",
                "--file",
                "services/hermes-manager/Dockerfile",
                "--build-arg",
                f"REVISION={revision}",
                "--tag",
                args.tag,
                str(context),
            ],
            cwd=context,
            timeout=1200,
            diagnostics=ROOT / ".local/hermes-manager-build.log",
        )
    print("Built the manager and protected dashboard image locally.")


if __name__ == "__main__":
    (ROOT / ".local").mkdir(exist_ok=True)
    main()
