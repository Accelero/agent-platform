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
    assert len(images) == 5
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
    chart = release.prepare_chart(tmp_path, images, "0.4.1", "b" * 40)
    assert yaml.safe_load((chart / "values.yaml").read_text())["images"] == images
    definition = yaml.safe_load((chart / "Chart.yaml").read_text())
    assert definition["version"] == "0.4.1"
    assert definition["annotations"]["org.opencontainers.image.revision"] == "b" * 40
    assert (chart / "templates/_runtime.tpl").is_file()
    assert (chart / "files/multica-gateway.nginx.conf").is_file()
    assert not (tmp_path / "contract.zip").exists()
    assert (release.ROOT / "charts/agent-platform/values.yaml").read_bytes() == original


@pytest.mark.parametrize(
    "version", ["latest", "v0.1.0", "01.1.0", "../escape", "0.1", "0.1.0-beta"]
)
def test_invalid_version_never_runs_build_commands(version):
    with patch.object(release, "run") as run:
        with pytest.raises(ValueError, match="semantic version"):
            release.build(version, "a" * 40, publish=True)
        run.assert_not_called()


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
            "0.4.1", "a" * 40, publish=True, github_cache=github_cache
        )
    assert any("type=gha" in value for value in commands[0]) == github_cache
    assert not any("type=registry" in value for value in commands[0])
    assert receipt["schema"] == 4
    assert receipt["version"] == "0.4.1"
    assert receipt["source"]["repository"] == release.REPOSITORY
    assert "contract_sha256" not in receipt
    assert not (tmp_path / "0.4.1/contract.zip").exists()
    assert commands[-1][:2] == ["helm", "push"]
    assert not any(command[:3] == ["docker", "buildx", "build"] for command in commands)
    promotions = [
        command
        for command in commands
        if command[:3] == ["docker", "buildx", "imagetools"]
    ]
    assert len(promotions) == 5
    assert all(
        not any("pulumi" in part.lower() for part in command) for command in commands
    )


def workflow(name: str) -> dict:
    return yaml.safe_load((release.ROOT / f".github/workflows/{name}.yml").read_text())


def commands(job: dict) -> str:
    return "\n".join(step.get("run", "") for step in job.get("steps", []))


def test_publishing_only_runs_for_version_tags_on_hosted_runners():
    jobs = workflow("release")["jobs"]
    assert list(workflow("release")["on"]) == ["push"]
    assert workflow("release")["on"]["push"]["tags"]
    assert workflow("release")["permissions"] == {"contents": "read"}
    publish = jobs["publish"]
    assert publish["runs-on"] == "ubuntu-latest"
    assert publish["permissions"]["packages"] == "write"
    assert "pulumi" not in commands(publish).lower()
    assert "GITHUB_REF_NAME" in commands(publish)
    assert "GITHUB_REF_NAME" in commands(jobs["validate"])


def test_every_suite_passes_before_a_release_publishes():
    jobs = workflow("release")["jobs"]
    assert jobs["tests"]["uses"] == "./.github/workflows/tests.yml"
    assert jobs["tests"]["with"] == {"suites": "all"}
    assert jobs["tests"]["permissions"] == {"contents": "read"}
    assert "tests" in jobs["publish"]["needs"]
    suite = workflow("tests")["jobs"]["suite"]
    assert suite["strategy"]["fail-fast"] is False
    assert '--suite "$SUITE" --shard "$SHARD"' in commands(suite)


def test_main_and_pull_requests_run_no_tests():
    """Only release tags run tests; main and pull requests check collection."""
    for name in ("checks", "cache"):
        for job in workflow(name)["jobs"].values():
            assert "uses" not in job
            for line in commands(job).splitlines():
                if "pytest" in line:
                    assert "--collect-only" in line, (name, line)
    assert "main" in workflow("cache")["on"]["push"]["branches"]
