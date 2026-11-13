"""Supply the release images under test, built like a release or given by reference."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from collections.abc import Callable
from pathlib import Path

import pytest

from agent_platform.build import ROOT, TARGETS, bake_arguments
from agent_platform.command import run
from agent_platform.privacy import stage_contexts

SUPPLIED = "AGENT_PLATFORM_TEST_IMAGES"
CACHE = "AGENT_PLATFORM_TEST_CACHE"


def require_docker(*tools: str) -> None:
    """Skip without Docker or a host tool; the suite plugin fails skips in CI."""
    for tool in ("docker", *tools):
        if shutil.which(tool) is None:
            pytest.skip(f"{tool} is required for image tests")
    result = subprocess.run(
        ["docker", "info"], capture_output=True, timeout=30, check=False
    )
    if result.returncode != 0:
        pytest.skip("a running Docker daemon is required for image tests")


def supplied() -> dict[str, str] | None:
    value = os.environ.get(SUPPLIED)
    if not value:
        return None
    images = json.loads(value)
    if not isinstance(images, dict) or not set(images) <= set(TARGETS):
        raise pytest.UsageError(f"{SUPPLIED} must map bake targets to references")
    return images


def build(target: str, stage: Path) -> str:
    """Build one bake target from the reviewed staged contexts and load it."""
    require_docker()
    contexts = stage_contexts(ROOT, stage / "inputs")
    tag = f"agent-platform-test/{target}:local"
    args = bake_arguments(
        github_cache=os.environ.get(CACHE) == "gha",
        cache_write=False,
        contexts=contexts,
        targets=(target,),
    )
    # A loaded test image is never published, so it carries no attestation; the
    # docker exporter cannot load the manifest list an attestation creates.
    args = [
        "--provenance=false" if arg.startswith("--provenance") else arg for arg in args
    ]
    # The staged contexts live outside the checkout; grant bake only that read.
    args += ["--allow", f"fs.read={stage}", "--load", "--set", f"{target}.tags={tag}"]
    log = stage / "build.log"
    try:
        run(args, cwd=ROOT, timeout=3600, diagnostics=log)
    except RuntimeError as error:
        tail = log.read_text(errors="replace")[-8000:] if log.exists() else ""
        pytest.fail(f"Building {target} failed: {error}\n{tail}")
    return tag


@pytest.fixture(scope="session")
def platform_image(tmp_path_factory) -> Callable[[str], str]:
    """Return `image(target)`: a release image reference, built once per session."""
    given = supplied()
    built: dict[str, str] = {}
    failed: dict[str, str] = {}

    def image(target: str) -> str:
        assert target in TARGETS, target
        if given is not None:
            if target not in given:
                pytest.fail(f"{SUPPLIED} names no {target} image")
            return given[target]
        if target in failed:
            pytest.fail(f"Building {target} already failed in this session")
        if target not in built:
            try:
                built[target] = build(target, tmp_path_factory.mktemp(target))
            except pytest.fail.Exception:
                failed[target] = target
                raise
        return built[target]

    return image


def run_in_image(
    image: str,
    *command: str,
    payload: Path | None = None,
    options: tuple[str, ...] = (),
    timeout: float = 180,
) -> str:
    """Run a command offline in an image, feeding an optional payload on stdin."""
    require_docker()
    entrypoint, *arguments = command
    result = subprocess.run(
        [
            "docker",
            "run",
            "--rm",
            "--network",
            "none",
            *(["--interactive"] if payload else []),
            *options,
            "--entrypoint",
            entrypoint,
            image,
            *arguments,
        ],
        input=payload.read_bytes() if payload else None,
        capture_output=True,
        timeout=timeout,
        check=False,
    )
    output = (result.stdout + result.stderr).decode(errors="replace")
    assert result.returncode == 0, output[-6000:]
    return output
