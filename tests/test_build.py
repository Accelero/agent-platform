"""Protect cache isolation, credential handling and release publication boundaries."""

from unittest.mock import patch

import pytest
import yaml

from agent_platform import build, release


@pytest.fixture
def github_runtime(monkeypatch):
    monkeypatch.setenv("GITHUB_ACTIONS", "true")
    monkeypatch.setenv("ACTIONS_RUNTIME_TOKEN", "fixture-runtime-credential")
    monkeypatch.setenv("ACTIONS_RESULTS_URL", "https://fixture.invalid/cache")


def test_github_cache_scopes_are_independent_and_credentials_stay_in_environment(
    github_runtime,
):
    args = build.bake_arguments(github_cache=True)
    imports = [value for value in args if ".cache-from=" in value]
    exports = [value for value in args if ".cache-to=" in value]
    assert len(imports) == len(exports) == len(build.TARGETS)
    assert len({value.split("scope=")[1] for value in imports}) == len(build.TARGETS)
    assert all("type=gha,version=2" in value for value in imports + exports)
    assert not any("fixture-runtime-credential" in value for value in args)
    assert not any("fixture.invalid" in value for value in args)
    assert not any(
        "type=registry" in value or "-cache:build" in value for value in args
    )


def test_local_builds_do_not_require_runner_credentials(monkeypatch):
    monkeypatch.delenv("ACTIONS_RUNTIME_TOKEN", raising=False)
    args = build.bake_arguments()
    assert not any("cache-from=" in value or "cache-to=" in value for value in args)


def test_missing_cache_runtime_stops_before_external_operations(monkeypatch):
    monkeypatch.setenv("GITHUB_ACTIONS", "true")
    monkeypatch.delenv("ACTIONS_RUNTIME_TOKEN", raising=False)
    with patch.object(build, "run") as run:
        with pytest.raises(ValueError, match="runner runtime"):
            build.warm_cache()
        run.assert_not_called()


def test_main_cache_build_never_pushes_or_loads_images(github_runtime, tmp_path):
    def run(args, **kwargs):
        kwargs["diagnostics"].write_bytes(b"#1 CACHED\n#2 CACHED\n")
        return b""

    with (
        patch.object(build, "ROOT", tmp_path),
        patch.object(build, "stage_contexts", return_value={}),
        patch.object(build, "run", side_effect=run) as command,
    ):
        build.warm_cache()
    args = command.call_args.args[0]
    assert "*.output=type=cacheonly" in args
    assert "--push" not in args
    assert "--load" not in args
    assert not any(".tags=" in value for value in args)
    assert command.call_count == 1


def test_cache_workflow_is_main_only_and_cannot_publish_packages():
    workflow = yaml.safe_load((build.ROOT / ".github/workflows/cache.yml").read_text())
    assert workflow["on"]["push"]["branches"] == ["main"]
    assert workflow["jobs"]["cache"]["if"] == "github.ref == 'refs/heads/main'"
    assert workflow["permissions"] == {"contents": "read"}
    steps = workflow["jobs"]["cache"]["steps"]
    commands = "\n".join(step.get("run", "") for step in steps)
    assert "python -m agent_platform.build" in commands
    assert "--publish" not in commands
    assert "--push" not in commands
    assert not any("login-action" in step.get("uses", "") for step in steps)


def test_release_workflow_uses_the_shared_cache_configuration():
    workflow = yaml.safe_load(
        (build.ROOT / ".github/workflows/release.yml").read_text()
    )
    commands = "\n".join(
        step.get("run", "") for step in workflow["jobs"]["publish"]["steps"]
    )
    assert "--publish --github-cache" in commands
    assert release.TARGETS == build.TARGETS


@pytest.mark.parametrize(
    ("pattern", "version"),
    [
        ("runtimes/hermes/*.py", (3, 13)),
        ("runtimes/hermes/plugins/managed_execution/*.py", (3, 13)),
    ],
)
def test_image_code_parses_on_its_image_python(pattern, version):
    """Code runs on each image's interpreter, not the 3.14 used for development."""
    import ast

    files = sorted(build.ROOT.glob(pattern))
    assert files
    for path in files:
        ast.parse(path.read_text(), filename=str(path), feature_version=version)
