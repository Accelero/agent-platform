"""Exercise public release metadata without Docker, network or deployment secrets."""

import hashlib
import json
import zipfile
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
    assert len(images) == 5
    assert all("@sha256:" in value for value in images.values())
    metadata["hermes"] = {"containerimage.config.digest": "sha256:" + "b" * 64}
    with pytest.raises(ValueError, match="registry digest"):
        release.image_references(metadata, release.REGISTRY)


def test_contract_is_small_deterministic_and_has_only_consumer_inputs(tmp_path):
    first = release.contract(tmp_path).read_bytes()
    assert release.contract(tmp_path).read_bytes() == first
    with zipfile.ZipFile(release.contract(tmp_path)) as bundle:
        assert set(bundle.namelist()) == {"values.yaml", "configuration.py"}
        assert len(first) < 100_000
        assert yaml.safe_load(bundle.read("values.yaml"))["workloads"]
        assert b"def load_policy" in bundle.read("configuration.py")


@pytest.mark.parametrize(
    "version", ["latest", "v0.1.0", "01.1.0", "../escape", "0.1", "0.1.0-beta"]
)
def test_invalid_version_never_runs_build_commands(version):
    with patch.object(release, "run") as run:
        with pytest.raises(ValueError, match="semantic version"):
            release.build(version, "a" * 40, publish=True)
        run.assert_not_called()


def test_metadata_latest_is_published_after_validation_and_chart(tmp_path):
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
        patch.object(release, "run", side_effect=run),
        patch.object(release, "render_chart"),
    ):
        receipt = release.build("0.1.0", "a" * 40, publish=True)
    assert receipt["schema"] == 3
    assert receipt["version"] == "0.1.0"
    assert receipt["source"]["repository"] == release.REPOSITORY
    assert (
        receipt["contract_sha256"]
        == hashlib.sha256((tmp_path / "0.1.0/contract.zip").read_bytes()).hexdigest()
    )
    assert commands[-1][:3] == ["docker", "buildx", "build"]
    assert release.REGISTRY + "/release:latest" in commands[-1]
    assert "--provenance=false" in commands[-1]
    assert any(command[:2] == ["helm", "push"] for command in commands[:-1])
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
