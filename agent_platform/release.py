"""Build and publish versioned agent artifacts without deployment credentials."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

import yaml

from agent_platform.build import ROOT, TARGETS, bake_arguments
from agent_platform.command import run
from agent_platform.privacy import chart_files, stage_contexts

OUTPUT = ROOT / ".local/releases"
REPOSITORY = "https://github.com/Accelero/agent-platform"
REGISTRY = "ghcr.io/accelero/agent-platform"
CHART_REGISTRY = "oci://" + REGISTRY + "/charts"
DIGEST = re.compile(r"sha256:[a-f0-9]{64}")
VERSION = re.compile(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)")


def image_references(metadata: dict, registry: str) -> dict[str, str]:
    """Require registry manifest digests instead of local image IDs or tags."""
    images = {}
    for name in TARGETS:
        digest = metadata.get(name, {}).get("containerimage.digest", "")
        if not isinstance(digest, str) or not DIGEST.fullmatch(digest):
            raise ValueError("Published image metadata lacks a registry digest.")
        images[name] = f"{registry}/{name}@{digest}"
    return images


def prepare_chart(
    target: Path, images: dict[str, str], version: str, revision: str
) -> Path:
    """Package all executable wiring and exact runtime pins in the chart itself."""
    chart = target / "chart/agent-platform"
    for name in chart_files(ROOT):
        source = ROOT / "charts/agent-platform" / name
        destination = chart / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(source.read_bytes())
    values = yaml.safe_load((chart / "values.yaml").read_text())
    values["images"] = images
    (chart / "values.yaml").write_text(yaml.safe_dump(values, sort_keys=False))
    definition = yaml.safe_load((chart / "Chart.yaml").read_text())
    definition.update(version=version, appVersion=version)
    definition["annotations"].update(
        {
            "org.opencontainers.image.revision": revision,
            "agent-platform.io/configuration-version": "2",
        }
    )
    (chart / "Chart.yaml").write_text(yaml.safe_dump(definition, sort_keys=False))
    return chart


def render_chart(target: Path, chart: Path) -> None:
    """Exercise a generic lightweight example without personal deployment inputs."""
    text = (ROOT / "examples/lightweight/values.yaml").read_text()
    values = target / "reviewed-values.yaml"
    values.write_text(text)
    output = run(
        [
            "helm",
            "template",
            "agents",
            str(chart),
            "--namespace",
            "agents",
            "--values",
            str(values),
        ],
        timeout=60,
    )
    documents = [item for item in yaml.safe_load_all(output) if item]
    deployed = {
        item["metadata"]["name"]
        for item in documents
        if item["kind"] in {"Deployment", "StatefulSet", "DaemonSet"}
    }
    if deployed != {
        "hermes",
        "runtime-manager",
        "multica-backend",
        "multica-web",
        "multica-gateway",
        "hermes-gateway",
    }:
        raise ValueError(
            "The lightweight example contains deferred or missing workloads."
        )
    for item in documents:
        if item["kind"] == "PersistentVolumeClaim":
            raise ValueError("The lightweight example must consume existing claims.")
        if item["kind"] == "Deployment":
            volumes = item["spec"]["template"]["spec"].get("volumes", [])
            if len({value["name"] for value in volumes}) != len(volumes):
                raise ValueError("The chart generated duplicate volume names.")
    (target / "reviewed-manifests.yaml").write_bytes(output)


def build(
    version: str,
    revision: str,
    *,
    publish: bool,
    registry: str = REGISTRY,
    chart_registry: str = CHART_REGISTRY,
    github_cache: bool = False,
) -> dict:
    """Publish images first and their self-contained versioned chart last."""
    if not VERSION.fullmatch(version) or not re.fullmatch(r"[a-f0-9]{40}", revision):
        raise ValueError("Use a complete semantic version and source commit SHA.")
    if not re.fullmatch(r"ghcr\.io/[a-z0-9][a-z0-9/_-]*", registry) or not re.fullmatch(
        r"oci://ghcr\.io/[a-z0-9][a-z0-9/_-]*", chart_registry
    ):
        raise ValueError("Use explicit lowercase GHCR namespaces.")
    target = OUTPUT / version
    target.mkdir(parents=True, exist_ok=True)
    contexts = stage_contexts(ROOT, target / "contexts")
    metadata = target / "images-metadata.json"
    args = [
        *bake_arguments(github_cache=github_cache, contexts=contexts),
        "--metadata-file",
        str(metadata),
        "--push" if publish else "--load",
        "--set",
        "*.labels.org.opencontainers.image.revision=" + revision,
        "--set",
        "*.labels.org.opencontainers.image.version=" + version,
    ]
    for name in TARGETS:
        args.extend(["--set", f"{name}.tags={registry}/{name}:{version}"])
    sys.stdout.write("Building agent release " + version + ".\n")
    sys.stdout.flush()
    run(args, cwd=ROOT, timeout=5400, diagnostics=target / "build.log")
    images = (
        image_references(json.loads(metadata.read_text()), registry)
        if publish
        else {name: f"{registry}/{name}:{version}" for name in TARGETS}
    )
    sys.stdout.write("Verifying built runtime images.\n")
    sys.stdout.flush()
    run(
        [
            "docker",
            "run",
            "--rm",
            "--network",
            "none",
            "--interactive",
            "--entrypoint",
            "python",
            images["runtime-manager"],
            "-",
        ],
        data=(ROOT / "tests/runtime_smoke.py").read_bytes(),
        timeout=180,
    )
    run(
        [
            "docker",
            "run",
            "--rm",
            "--network",
            "none",
            "--entrypoint",
            "python",
            images["tools"],
            "-c",
            (
                "import boto3,kubernetes,yaml; import subprocess; "
                'subprocess.run(["pg_dump","--version"],check=True); '
                'subprocess.run(["restic","version"],check=True)'
            ),
        ],
        timeout=180,
    )
    sys.stdout.write("Validating chart.\n")
    sys.stdout.flush()
    chart = prepare_chart(target, images, version, revision)
    run(["helm", "lint", str(chart)], timeout=60)
    render_chart(target, chart)
    run(
        [
            "helm",
            "package",
            str(chart),
            "--version",
            version,
            "--app-version",
            version,
            "--destination",
            str(target),
        ],
        timeout=60,
    )
    package = target / ("agent-platform-" + version + ".tgz")
    receipt = {
        "schema": 4,
        "source": {"repository": REPOSITORY, "revision": revision},
        "version": version,
        "images": images,
    }
    if publish:
        for name, image in images.items():
            run(
                [
                    "docker",
                    "buildx",
                    "imagetools",
                    "create",
                    "--tag",
                    f"{registry}/{name}:latest",
                    image,
                ],
                timeout=180,
            )
    if publish:
        output = run(
            ["helm", "push", str(package), chart_registry],
            timeout=180,
            include_stderr=True,
        )
        matches = re.findall(rb"Digest:\s*(sha256:[a-f0-9]{64})", output)
        if len(matches) != 1:
            raise ValueError("Helm did not return one chart manifest digest.")
        receipt["chart"] = chart_registry + "/agent-platform@" + matches[0].decode()
    else:
        receipt["chart"] = str(package.relative_to(ROOT))
    (target / "artifacts.json").write_text(json.dumps(receipt, indent=2) + "\n")
    return receipt


def main() -> int:
    """Require an explicit release version; infrastructure never invokes builds."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--registry", default=REGISTRY)
    parser.add_argument("--chart-registry", default=CHART_REGISTRY)
    parser.add_argument("--publish", action="store_true")
    parser.add_argument("--github-cache", action="store_true")
    args = parser.parse_args()
    try:
        receipt = build(
            args.version,
            args.revision,
            publish=args.publish,
            registry=args.registry,
            chart_registry=args.chart_registry,
            github_cache=args.github_cache,
        )
    except OSError, ValueError, RuntimeError:
        sys.stderr.write("Agent release failed; private command output suppressed.\n")
        return 1
    sys.stdout.write(json.dumps(receipt, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
