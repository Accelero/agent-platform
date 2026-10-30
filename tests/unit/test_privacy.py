"""Reject private inputs before publication and keep build contexts finite."""

import json
from pathlib import Path
from unittest.mock import patch

import pytest
import yaml

from agent_platform import build, privacy, release

pytestmark = pytest.mark.unit


@pytest.fixture
def inputs(tmp_path):
    root = tmp_path / "source"
    chart = root / "charts/agent-platform"
    chart.mkdir(parents=True)
    (chart / "Chart.yaml").write_text("name: agent-platform\n")
    (chart / "values.yaml").write_text("ownerEmail: owner@example.invalid\n")
    (root / "operation.py").write_text("print('generic operation')\n")
    policy = {
        "chart_files": ["Chart.yaml", "values.yaml"],
        "contexts": {"tools": {"root": ".", "files": ["operation.py"]}},
    }
    (root / "release-policy.json").write_text(json.dumps(policy))
    return root, tmp_path / "staged"


def test_local_manager_context_is_checked_without_becoming_a_release_target(
    inputs, monkeypatch, capsys
):
    root, target = inputs
    manifest = json.loads((root / "release-policy.json").read_text())
    manifest["development_contexts"] = {
        "hermes-manager": {"root": ".", "files": ["operation.py"]}
    }
    (root / "release-policy.json").write_text(json.dumps(manifest))
    monkeypatch.setattr(privacy, "ROOT", root)
    assert privacy.main() == 0
    assert set(privacy.stage_contexts(root, target)) == {"tools"}
    (root / "operation.py").write_text("-----BEGIN PRIVATE KEY-----")
    assert privacy.main() == 1
    assert "BEGIN PRIVATE KEY" not in capsys.readouterr().err


def test_untracked_private_prompts_and_credentials_never_enter_contexts(inputs):
    root, target = inputs
    private = root / "config/agents"
    private.mkdir(parents=True)
    (private / "SOUL.md").write_text("Private personal instructions")
    (root / ".env").write_text("PRIVATE_VALUE=local-only")
    (root / "auth.json").write_text('{"private": true}')
    contexts = privacy.stage_contexts(root, target)
    assert set(contexts) == {"tools"}
    assert sorted(path.name for path in contexts["tools"].rglob("*")) == [
        "operation.py"
    ]


@pytest.mark.parametrize(
    "payload",
    [
        b"ghp_" + b"A" * 36,
        b"sk-proj-" + b"B" * 40,
        b"-----BEGIN " + b"PRIVATE KEY-----",
        b"alice@company.com",
    ],
    ids=["github-token", "provider-token", "private-key", "personal-identity"],
)
def test_sensitive_payload_fails_before_staging_without_echoing_values(inputs, payload):
    root, target = inputs
    (root / "operation.py").write_bytes(payload)
    with pytest.raises(privacy.PrivacyError) as error:
        privacy.stage_contexts(root, target)
    assert payload.decode() not in str(error.value)
    assert not target.exists()


def test_unreviewed_chart_prompt_fails_before_packaging(inputs):
    root, target = inputs
    (root / "charts/agent-platform/SOUL.md").write_text("Private prompt")
    with pytest.raises(privacy.PrivacyError, match="unreviewed"):
        privacy.stage_contexts(root, target)
    assert not target.exists()


def test_inline_chart_identity_is_rejected(inputs):
    root, target = inputs
    (root / "charts/agent-platform/values.yaml").write_text(
        "hermes:\n  system_prompt: private personal instructions\n"
    )
    with pytest.raises(privacy.PrivacyError, match="prompts or identities"):
        privacy.stage_contexts(root, target)
    assert not target.exists()


def test_a_private_prompt_cannot_be_added_to_the_context_allowlist(inputs):
    root, target = inputs
    policy = json.loads((root / "release-policy.json").read_text())
    policy["contexts"]["tools"]["files"].append("SOUL.md")
    (root / "release-policy.json").write_text(json.dumps(policy))
    (root / "SOUL.md").write_text("Private identity")
    with pytest.raises(privacy.PrivacyError, match="private configuration or prompt"):
        privacy.stage_contexts(root, target)


@pytest.mark.parametrize("directory", ["config/agents", "profiles/personal-assistant"])
def test_private_settings_remain_forbidden_when_context_root_changes(inputs, directory):
    root, target = inputs
    policy = json.loads((root / "release-policy.json").read_text())
    policy["contexts"]["tools"] = {"root": directory, "files": ["config.yaml"]}
    (root / "release-policy.json").write_text(json.dumps(policy))
    settings = root / directory
    settings.mkdir(parents=True)
    (settings / "config.yaml").write_text("model: private-preference\n")
    with pytest.raises(privacy.PrivacyError, match="private configuration or prompt"):
        privacy.stage_contexts(root, target)


def test_release_inputs_cannot_link_to_private_neighbor_files(inputs):
    root, target = inputs
    (root / "operation.py").unlink()
    outside = root.parent / "private.py"
    outside.write_text("Private material")
    (root / "operation.py").symlink_to(outside)
    with pytest.raises(privacy.PrivacyError, match="symbolic links"):
        privacy.stage_contexts(root, target)


def test_stale_context_cannot_be_reused(inputs):
    root, target = inputs
    target.mkdir()
    (target / "private.env").write_text("private")
    with pytest.raises(privacy.PrivacyError, match="start empty"):
        privacy.stage_contexts(root, target)


def test_bake_overrides_every_staged_context():
    args = build.bake_arguments(
        contexts={
            "hermes": Path("/reviewed/hermes"),
            "hermes-manager": Path("/reviewed/hermes-manager"),
        }
    )
    assert "hermes.context=/reviewed/hermes" in args
    assert "hermes-manager.context=/reviewed/hermes-manager" in args


def test_privacy_failure_prevents_all_build_and_publication_commands(tmp_path):
    with (
        patch.object(release, "OUTPUT", tmp_path),
        patch.object(
            release, "stage_contexts", side_effect=privacy.PrivacyError("blocked")
        ),
        patch.object(release, "run") as run,
    ):
        with pytest.raises(privacy.PrivacyError):
            release.build("0.1.0", "a" * 40, publish=True)
    run.assert_not_called()


def test_example_email_and_digest_references_are_not_personal_data():
    privacy.check_content(b"owner@example.invalid git@github.com hermes@localhost")
    privacy.check_content(b"ghcr.io/example/image@sha256:" + b"a" * 64)


def test_build_attestations_do_not_copy_the_github_event_payload():
    assert "--provenance=mode=min" in build.bake_arguments()
    for name in ("release", "cache"):
        workflow = yaml.safe_load(
            (build.ROOT / f".github/workflows/{name}.yml").read_text()
        )
        steps = [step for job in workflow["jobs"].values() for step in job["steps"]]
        builder = next(step for step in steps if step.get("name") == "Set up BuildKit")
        assert builder["with"]["driver-opts"] == "provenance-add-gha=false"
