"""Keep personal configuration, prompts and credentials outside release inputs."""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[1]
_FILE_LIMIT = 16 * 1024 * 1024
_TOTAL_LIMIT = 128 * 1024 * 1024
_CREDENTIALS = {
    "private-key": rb"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----",
    "github-token": rb"\b(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{50,})\b",
    "provider-key": rb"\bsk-(?:proj-|svcacct-)?[A-Za-z0-9_-]{32,}\b",
    "tailscale-key": rb"\btskey-(?:auth|api|client)-[A-Za-z0-9_-]{20,}\b",
    "aws-key": rb"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b",
}
_EMAIL = re.compile(
    rb"[A-Za-z0-9_.+-]+@(?:(?:[A-Za-z0-9-]+\.)+[A-Za-z0-9-]+|localhost)\b"
)
_PRIVATE_NAMES = {
    "soul.md",
    "user.md",
    "identity.md",
    "memory.md",
    "instructions.md",
    "skill.md",
    "auth.json",
    "credentials.json",
    "kubeconfig",
    "id_rsa",
    "id_ed25519",
}
_PRIVATE_DIRECTORIES = {
    "profiles",
    "skills",
    "memories",
    "sessions",
    ".hermes",
    ".codex",
    ".ssh",
    ".git",
}
_PRIVATE_KEYS = {
    "prompt",
    "systemprompt",
    "instructions",
    "persona",
    "soul",
    "userprofile",
}


class PrivacyError(ValueError):
    """Report release policy failures without echoing private content."""


def _path(root: Path, name: str) -> Path:
    relative = Path(name)
    if relative.is_absolute() or ".." in relative.parts:
        raise PrivacyError("Release input escapes its reviewed directory.")
    path = root / relative
    if path.is_symlink() or any(parent.is_symlink() for parent in path.parents):
        raise PrivacyError("Release inputs cannot traverse symbolic links.")
    return path


def _data(root: Path, name: str) -> bytes:
    path = _path(root, name)
    if not path.is_file() or path.stat().st_size > _FILE_LIMIT:
        raise PrivacyError("A reviewed release input is absent or exceeds its limit.")
    return path.read_bytes()


def check_content(payload: bytes) -> None:
    """Reject recognizable credentials and non-example personal email addresses."""
    for rule, expression in _CREDENTIALS.items():
        if re.search(expression, payload):
            raise PrivacyError(f"Release privacy check failed: {rule}.")
    for match in _EMAIL.finditer(payload):
        address = match.group().decode().lower()
        domain = address.rsplit("@", 1)[1]
        if (
            domain == "localhost"
            or domain in {"example.com", "example.org", "example.net"}
            or domain.endswith((".example", ".test", ".invalid"))
            or address in {"git@github.com", "git@gitlab.com", "git@bitbucket.org"}
        ):
            continue
        raise PrivacyError("Release privacy check failed: personal-email.")


def _private_values(value: object) -> None:
    if isinstance(value, dict):
        for key, child in value.items():
            normalized = str(key).replace("_", "").replace("-", "").lower()
            if normalized in _PRIVATE_KEYS and child:
                raise PrivacyError(
                    "Chart defaults cannot contain prompts or identities."
                )
            _private_values(child)
    elif isinstance(value, list):
        for child in value:
            _private_values(child)


def policy(root: Path) -> dict:
    """Read the reviewed finite lists, independent of Git's tracked/ignored state."""
    return json.loads(_data(root, "release-policy.json"))


def chart_files(root: Path) -> list[str]:
    """Refuse unexpected chart files instead of silently packaging local state."""
    names = policy(root)["chart_files"]
    chart = root / "charts/agent-platform"
    actual = {
        str(path.relative_to(chart))
        for path in chart.rglob("*")
        if path.is_file() or path.is_symlink()
    }
    if actual != set(names):
        raise PrivacyError("Chart contains unreviewed or missing release files.")
    for name in names:
        if Path(name).name.lower() in _PRIVATE_NAMES:
            raise PrivacyError("Chart includes a private configuration or prompt path.")
        check_content(_data(chart, name))
    try:
        _private_values(yaml.safe_load(_data(chart, "values.yaml")))
    except yaml.YAMLError:
        raise PrivacyError(
            "Chart defaults are malformed; content suppressed."
        ) from None
    return names


def context_files(root: Path, definition: dict) -> dict[str, bytes]:
    """Read the explicitly listed, reviewed payloads of one build context."""
    base = _path(root, definition["root"])
    result = {}
    total = 0
    for name in definition["files"]:
        source = Path(definition["root"]) / name
        if (
            Path(name).name.lower() in _PRIVATE_NAMES
            or Path(name).name.startswith(".env")
            or _PRIVATE_DIRECTORIES.intersection(source.parts)
            or source.parts[:2] == ("config", "agents")
        ):
            raise PrivacyError(
                "Build context includes a private configuration or prompt path."
            )
        payload = _data(base, name)
        total += len(payload)
        if total > _TOTAL_LIMIT:
            raise PrivacyError("Reviewed build context exceeds its size limit.")
        check_content(payload)
        result[name] = payload
    return result


def stage_contexts(root: Path, destination: Path) -> dict[str, Path]:
    """Give BuildKit fresh finite contexts containing only approved input bytes."""
    if destination.exists() and any(destination.iterdir()):
        raise PrivacyError("Release context staging must start empty.")
    reviewed = policy(root)
    payloads = {
        name: context_files(root, definition)
        for name, definition in reviewed["contexts"].items()
    }
    chart_files(root)
    contexts = {}
    for name, files in payloads.items():
        target = destination / name
        for relative, payload in files.items():
            path = target / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(payload)
            source = root / reviewed["contexts"][name]["root"] / relative
            path.chmod(source.stat().st_mode & 0o777)
        contexts[name] = target
    return contexts


def main() -> int:
    """Run source checks without printing matched values or private file content."""
    try:
        reviewed = policy(ROOT)
        chart_files(ROOT)
        for definition in reviewed["contexts"].values():
            context_files(ROOT, definition)
        for definition in reviewed.get("development_contexts", {}).values():
            context_files(ROOT, definition)
    except OSError, ValueError, KeyError, TypeError:
        sys.stderr.write(
            "Release privacy preflight failed; private details suppressed.\n"
        )
        return 1
    print("Reviewed chart/build inputs passed release privacy preflight.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
