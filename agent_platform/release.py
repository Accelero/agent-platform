"""Build and publish versioned agent artifacts without deployment credentials."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import sys
import zipfile
from pathlib import Path

import yaml

from agent_platform.command import run

ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / ".local/releases"
TARGETS = ("hermes", "execution-manager", "worker", "multica-backend", "state")
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


def contract(target: Path) -> Path:
    """Export only the data and policy adapter needed by deployment consumers."""
    path = target / "contract.zip"
    with zipfile.ZipFile(path, "w", compression=zipfile.ZIP_DEFLATED) as bundle:
        for name, source in {
            "values.yaml": ROOT / "charts/agent-platform/values.yaml",
            "configuration.py": ROOT / "services/execution-manager/configuration.py",
        }.items():
            entry = zipfile.ZipInfo(name, date_time=(2026, 1, 1, 0, 0, 0))
            entry.compress_type = zipfile.ZIP_DEFLATED
            entry.external_attr = 0o100644 << 16
            bundle.writestr(entry, source.read_bytes())
    return path


def render_chart(target: Path, images: dict[str, str]) -> None:
    """Exercise a generic lightweight example without personal deployment inputs."""
    text = (ROOT / "examples/lightweight/values.yaml").read_text()
    for name, image in images.items():
        text = text.replace(registry_placeholder(name), image)
    values = target / "reviewed-values.yaml"
    values.write_text(text)
    output = run(
        [
            "helm",
            "template",
            "agents",
            str(ROOT / "charts/agent-platform"),
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
    if deployed != {"hermes", "execution-manager", "multica-backend", "multica-web"}:
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


def registry_placeholder(name: str) -> str:
    """Identify the immutable dummy image references in the example fixture."""
    return "ghcr.io/fixture/agents/" + name + "@sha256:" + "a" * 64


def publish_metadata(target: Path, registry: str, version: str, revision: str) -> None:
    """Advance latest only after all matching runtime and chart artifacts exist."""
    context = target / "metadata"
    context.mkdir()
    for filename in ("artifacts.json", "contract.zip"):
        shutil.copyfile(target / filename, context / filename)
    (context / "Dockerfile").write_text(
        "FROM scratch\nCOPY artifacts.json contract.zip /\n"
    )
    run(
        [
            "docker",
            "buildx",
            "build",
            "--push",
            "--platform",
            "linux/amd64",
            "--provenance=false",
            "--sbom=false",
            "--label",
            "org.opencontainers.image.source=" + REPOSITORY,
            "--label",
            "org.opencontainers.image.revision=" + revision,
            "--label",
            "org.opencontainers.image.version=" + version,
            "--tag",
            registry + "/release:" + version,
            "--tag",
            registry + "/release:latest",
            str(context),
        ],
        timeout=180,
    )


def build(
    version: str,
    revision: str,
    *,
    publish: bool,
    registry: str = REGISTRY,
    chart_registry: str = CHART_REGISTRY,
) -> dict:
    """Publish one explicit semantic version and a coherent latest release bundle."""
    if not VERSION.fullmatch(version) or not re.fullmatch(r"[a-f0-9]{40}", revision):
        raise ValueError("Use a complete semantic version and source commit SHA.")
    if not re.fullmatch(r"ghcr\.io/[a-z0-9][a-z0-9/_-]*", registry) or not re.fullmatch(
        r"oci://ghcr\.io/[a-z0-9][a-z0-9/_-]*", chart_registry
    ):
        raise ValueError("Use explicit lowercase GHCR namespaces.")
    target = OUTPUT / version
    target.mkdir(parents=True, exist_ok=True)
    metadata = target / "images-metadata.json"
    args = [
        "docker",
        "buildx",
        "bake",
        "--file",
        "containers/docker-bake.hcl",
        *TARGETS,
        "--metadata-file",
        str(metadata),
        "--push" if publish else "--load",
        "--set",
        "*.platform=linux/amd64",
        "--set",
        "*.labels.org.opencontainers.image.revision=" + revision,
        "--set",
        "*.labels.org.opencontainers.image.version=" + version,
    ]
    for name in TARGETS:
        args.extend(["--set", f"{name}.tags={registry}/{name}:{version}"])
        if publish:
            cache = f"{registry}/{name}-cache:build"
            args.extend(
                [
                    "--set",
                    f"{name}.cache-from=type=registry,ref={cache}",
                    "--set",
                    f"{name}.cache-to=type=registry,ref={cache},mode=max",
                ]
            )
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
            images["execution-manager"],
            "-",
        ],
        data=(ROOT / "tests/worker_smoke.py").read_bytes(),
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
            images["state"],
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
    chart = ROOT / "charts/agent-platform"
    run(["helm", "lint", str(chart)], timeout=60)
    render_chart(target, images)
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
    bundle = contract(target)
    receipt = {
        "schema": 3,
        "source": {"repository": REPOSITORY, "revision": revision},
        "version": version,
        "deployment_contract": 1,
        "contract_sha256": hashlib.sha256(bundle.read_bytes()).hexdigest(),
        "images": images,
    }
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
        publish_metadata(target, registry, version, revision)
    return receipt


def main() -> int:
    """Require an explicit release version; infrastructure never invokes builds."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--registry", default=REGISTRY)
    parser.add_argument("--chart-registry", default=CHART_REGISTRY)
    parser.add_argument("--publish", action="store_true")
    args = parser.parse_args()
    try:
        receipt = build(
            args.version,
            args.revision,
            publish=args.publish,
            registry=args.registry,
            chart_registry=args.chart_registry,
        )
    except OSError, ValueError, RuntimeError:
        sys.stderr.write("Agent release failed; private command output suppressed.\n")
        return 1
    sys.stdout.write(json.dumps(receipt, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
