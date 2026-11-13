"""Render the source chart with real Helm, shared by chart-consuming suites."""

import subprocess
from pathlib import Path

import yaml

from agent_platform.build import ROOT

CHART = ROOT / "charts/agent-platform"
HINDSIGHT_IMAGE = (
    "ghcr.io/vectorize-io/hindsight-api:1.2.1@sha256:"
    "662109ece429871a81e036427fb93fbb09ba9dc4b01d320d356f114c0e1aedd8"
)


def render(
    tmp_path: Path,
    values: dict,
    *,
    valid: bool = True,
    include_namespaces: bool = False,
) -> dict | str:
    path = tmp_path / "values.yaml"
    path.write_text(yaml.safe_dump(values))
    result = subprocess.run(
        [
            "helm",
            "template",
            "agents",
            str(CHART),
            "--namespace",
            "agents",
            "--values",
            str(path),
        ],
        capture_output=True,
        text=True,
        timeout=30,
    )
    if not valid:
        assert result.returncode != 0
        return result.stderr
    assert result.returncode == 0, result.stderr
    documents = [item for item in yaml.safe_load_all(result.stdout) if item]
    if include_namespaces:
        return {
            (
                item["kind"],
                item["metadata"].get("namespace"),
                item["metadata"]["name"],
            ): item
            for item in documents
        }
    return {(item["kind"], item["metadata"]["name"]): item for item in documents}


def native(objects):
    return yaml.safe_load(
        objects["ConfigMap", "runtime-manager-config"]["data"]["config.yaml"]
    )


def policy(objects):
    item = next(
        value
        for (kind, name), value in objects.items()
        if kind == "ValidatingAdmissionPolicy" and not name.endswith("-delete")
    )
    return "\n".join(rule["expression"] for rule in item["spec"]["validations"])
