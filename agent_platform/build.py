"""Share image targets and GitHub Actions cache settings across CI builds."""

from __future__ import annotations

import os
import re
import sys
from pathlib import Path

from agent_platform.command import run

ROOT = Path(__file__).resolve().parents[1]
TARGETS = ("hermes", "runtime-manager", "agent-runtime", "multica-backend", "tools")


def bake_arguments(*, github_cache: bool = False) -> list[str]:
    """Keep cache credentials in the runner environment and isolate image scopes."""
    args = [
        "docker",
        "buildx",
        "bake",
        "--file",
        "containers/docker-bake.hcl",
        *TARGETS,
        "--progress",
        "plain",
        "--set",
        "*.platform=linux/amd64",
    ]
    if not github_cache:
        return args
    if os.environ.get("GITHUB_ACTIONS") != "true" or not all(
        os.environ.get(name)
        for name in ("ACTIONS_RUNTIME_TOKEN", "ACTIONS_RESULTS_URL")
    ):
        raise ValueError("GitHub Actions cache requires the exported runner runtime.")
    for name in TARGETS:
        cache = f"type=gha,version=2,scope=agent-platform-{name}-amd64"
        args.extend(
            [
                "--set",
                f"{name}.cache-from={cache}",
                "--set",
                f"{name}.cache-to={cache},mode=max",
            ]
        )
    return args


def warm_cache() -> None:
    """Populate the default-branch cache without publishing or loading images."""
    args = [*bake_arguments(github_cache=True), "--set", "*.output=type=cacheonly"]
    output = ROOT / ".local/cache"
    output.mkdir(parents=True, exist_ok=True)
    log = output / "build.log"
    print(
        "Building the five image caches without publishing release artifacts.",
        flush=True,
    )
    run(args, cwd=ROOT, timeout=3300, diagnostics=log)
    reused = len(re.findall(rb"(?m)^#\d+ CACHED\r?$", log.read_bytes()))
    print(f"GitHub Actions cache populated; reused {reused} build steps.", flush=True)


def main() -> int:
    """Run the trusted main-branch cache workflow."""
    try:
        warm_cache()
    except OSError, ValueError, RuntimeError:
        sys.stderr.write("Cache warming failed; private command output suppressed.\n")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
