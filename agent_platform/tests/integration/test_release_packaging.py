"""Release packaging through real processes: Git, Helm and bounded output."""

import json
from pathlib import Path

import pytest
import yaml

from agent_platform import release


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


def test_h12_packaged_chart_keeps_hindsight_digest_and_wiring(tmp_path):
    import subprocess

    from agent_platform.privacy import chart_files
    from tests.support.chart import HINDSIGHT_IMAGE

    images = {
        name: release.REGISTRY + "/" + name + "@sha256:" + "a" * 64
        for name in release.TARGETS
    }
    chart = release.prepare_chart(tmp_path, images, "0.4.1", "b" * 40)
    assert set(chart_files(release.ROOT)) == {
        str(path.relative_to(chart)) for path in chart.rglob("*") if path.is_file()
    }
    subprocess.run(
        ["helm", "package", str(chart), "--destination", str(tmp_path)],
        capture_output=True,
        text=True,
        check=True,
        timeout=30,
    )
    result = subprocess.run(
        [
            "helm",
            "template",
            "agents",
            str(tmp_path / "agent-platform-0.4.1.tgz"),
            "-f",
            str(release.ROOT / "examples/lightweight/values.yaml"),
            "-f",
            str(release.ROOT / "examples/hindsight/values.yaml"),
        ],
        capture_output=True,
        text=True,
        check=True,
        timeout=30,
    )
    objects = {
        (obj["kind"], obj["metadata"]["name"]): obj
        for obj in yaml.safe_load_all(result.stdout)
        if obj
    }
    api = objects["Deployment", "hindsight"]["spec"]["template"]["spec"]["containers"][
        0
    ]
    assert api["image"] == HINDSIGHT_IMAGE
    assert "HINDSIGHT_API_DATABASE_URL" in {item["name"] for item in api["env"]}
    assert "postgres://*|postgresql://*) exec /app/start-all.sh" in api["command"][2]
    seed = json.loads(objects["ConfigMap", "hermes-defaults"]["data"]["defaults.json"])
    assert seed["hindsight"]["bank_id"] == "default"
    assert seed["config"]["memory"]["provider"] == "hindsight"
    assert objects["NetworkPolicy", "hindsight-ingress"]["spec"]["ingress"]
