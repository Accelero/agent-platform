"""Exercise public release metadata without Docker, network or deployment secrets."""

import json
from pathlib import Path
from unittest.mock import patch

import pytest
import yaml

from agent_platform import release


def test_requires_real_registry_manifest_digests():
    metadata = {
        name: {"containerimage.digest": "sha256:" + "a" * 64}
        for name in release.TARGETS
    }
    images = release.image_references(metadata, release.REGISTRY)
    assert len(images) == len(release.TARGETS) == 3
    assert all("@sha256:" in value for value in images.values())
    metadata["hermes"] = {"containerimage.config.digest": "sha256:" + "b" * 64}
    with pytest.raises(ValueError, match="registry digest"):
        release.image_references(metadata, release.REGISTRY)


def test_packaged_chart_has_pins_and_wiring_without_modifying_source(tmp_path):
    images = {
        name: release.REGISTRY + "/" + name + "@sha256:" + "a" * 64
        for name in release.TARGETS
    }
    original = (release.ROOT / "charts/agent-platform/values.yaml").read_bytes()
    chart = release.prepare_chart(tmp_path, images, "0.1.0", "b" * 40)
    assert yaml.safe_load((chart / "values.yaml").read_text())["images"] == images
    definition = yaml.safe_load((chart / "Chart.yaml").read_text())
    assert definition["version"] == "0.1.0"
    assert definition["annotations"]["org.opencontainers.image.revision"] == "b" * 40
    assert (chart / "templates/hermes-manager.yaml").is_file()
    assert not (tmp_path / "contract.zip").exists()
    assert (release.ROOT / "charts/agent-platform/values.yaml").read_bytes() == original


@pytest.mark.parametrize(
    "version",
    ["latest", "v0.1.0", "01.1.0", "../escape", "0.1", "0.1.0-beta", "0.7.0-dev.01"],
)
def test_invalid_version_never_runs_build_commands(version):
    with patch.object(release, "run") as run:
        with pytest.raises(ValueError, match="semantic version"):
            release.build(version, "a" * 40, publish=True)
        run.assert_not_called()


def test_stable_and_development_versions_are_distinct():
    for version in ("0.6.9", "0.7.0", "0.7.0-dev.0", "0.7.0-dev.12"):
        assert release.VERSION.fullmatch(version), version
    assert not release.VERSION.fullmatch("0.7.0-rc.1")


@pytest.mark.parametrize("github_cache", [False, True])
def test_chart_is_published_last_without_a_custom_release_package(
    tmp_path, monkeypatch, github_cache
):
    monkeypatch.setenv("GITHUB_ACTIONS", "true")
    monkeypatch.setenv("ACTIONS_RUNTIME_TOKEN", "fixture-runtime-credential")
    monkeypatch.setenv("ACTIONS_RESULTS_URL", "https://fixture.invalid/cache")
    commands = []

    def run(args, **kwargs):
        commands.append(args)
        if args[:3] == ["docker", "buildx", "bake"]:
            metadata = Path(args[args.index("--metadata-file") + 1])
            metadata.write_text(
                json.dumps(
                    {
                        name: {"containerimage.digest": "sha256:" + "a" * 64}
                        for name in release.TARGETS
                    }
                )
            )
        if args[:2] == ["helm", "push"]:
            return b"Digest: sha256:" + b"b" * 64
        return b""

    with (
        patch.object(release, "OUTPUT", tmp_path),
        patch.object(release, "stage_contexts", return_value={}),
        patch.object(release, "run", side_effect=run),
        patch.object(release, "render_chart"),
    ):
        receipt = release.build(
            "0.1.0", "a" * 40, publish=True, github_cache=github_cache
        )
    assert any("type=gha" in value for value in commands[0]) == github_cache
    assert not any("type=registry" in value for value in commands[0])
    assert receipt["schema"] == 4
    assert receipt["version"] == "0.1.0"
    assert receipt["source"]["repository"] == release.REPOSITORY
    assert "contract_sha256" not in receipt
    assert not (tmp_path / "0.1.0/contract.zip").exists()
    assert commands[-1][:2] == ["helm", "push"]
    assert not any(command[:3] == ["docker", "buildx", "build"] for command in commands)
    promotions = [
        command
        for command in commands
        if command[:3] == ["docker", "buildx", "imagetools"]
    ]
    assert len(promotions) == len(release.TARGETS)
    assert all(
        not any("pulumi" in part.lower() for part in command) for command in commands
    )


def test_publishing_only_runs_for_version_tags_on_hosted_runners():
    workflow = yaml.safe_load(
        (release.ROOT / ".github/workflows/release.yml").read_text()
    )
    assert workflow["on"]["push"]["tags"]
    assert workflow["jobs"]["publish"]["runs-on"] == "ubuntu-latest"
    assert workflow["permissions"]["packages"] == "write"
    commands = "\n".join(
        step.get("run", "") for step in workflow["jobs"]["publish"]["steps"]
    )
    assert "pulumi" not in commands.lower()
    assert "GITHUB_REF_NAME" in commands


def test_failed_build_diagnostics_are_explicit_and_bounded(tmp_path):
    """Only opted-in generic build output is retained, with a strict size bound."""
    import sys

    from agent_platform.command import run

    path = tmp_path / "build.log"
    with pytest.raises(RuntimeError) as error:
        run(
            [
                sys.executable,
                "-c",
                (
                    'import sys; print("x" * 3_000_000); '
                    'print("build failed", file=sys.stderr); sys.exit(2)'
                ),
            ],
            diagnostics=path,
        )
    assert path.stat().st_size == 2_097_152
    assert path.read_bytes().endswith(b"build failed\n")
    assert "build failed" not in str(error.value)


def test_reviewed_build_sources_are_complete_and_not_ignored():
    """Generic credential ignore patterns must not drop required source files."""
    import subprocess

    manifest = json.loads((release.ROOT / "release-policy.json").read_text())
    names = []
    for context in manifest["contexts"].values():
        for relative in context.get("files", []):
            name = str(Path(context["root"]) / relative)
            assert (release.ROOT / name).is_file(), name
            names.append(name)
    result = subprocess.run(
        ["git", "check-ignore", "--no-index", "--stdin", "-z"],
        input=("\0".join(names) + "\0").encode(),
        capture_output=True,
        cwd=release.ROOT,
        timeout=10,
    )
    assert result.returncode == 1, result.stdout.decode()
