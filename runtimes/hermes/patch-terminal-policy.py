"""Guarded integration seams for personal terminal policy in pinned Hermes."""

import hashlib
import shutil
from pathlib import Path

ROOT = Path("/opt/hermes")
PATCHES = [
    (
        "hermes_cli/managed_scope.py",
        "692289b37fa3ce547b37d8fc20baf12f0e1a8358689ebdc48e51d06250e91037",
        '    override = os.environ.get("HERMES_MANAGED_DIR", "").strip()',
        "    from hermes_cli._platform_terminal_policy import managed_directory\n"
        "    return managed_directory()",
    ),
    (
        "tools/terminal_scope.py",
        "4ba7e1a357b4c6d1b58d506bff7dbc20a66b1b34163ec645dd817f9d3b2e4c35",
        "    scope = _terminal_scope_var.get()\n    if scope is None:",
        "    scope = _terminal_scope_var.get()\n"
        "    enforce_no_refusal()\n"
        "    from hermes_cli._platform_terminal_policy import terminal_override\n"
        "    pinned = terminal_override(name)\n"
        "    if pinned is not None:\n"
        "        return pinned\n"
        "    if scope is None:",
    ),
    (
        "tools/terminal_tool_backends.py",
        "d241563d0ce81f65beb9aaf74e10b812488fe4edd97b63198590ba8aad85381a",
        "    builder = _ENV_BUILDERS.get(env_type)",
        "    from hermes_cli._platform_terminal_policy import enforce_backend\n"
        "    enforce_backend(env_type)\n"
        "    builder = _ENV_BUILDERS.get(env_type)",
    ),
    (
        "hermes_cli/plugins_discovery.py",
        "518504306fe937bae73a10f24bcfb040d81b2579cfb6115ef7fccd3c28d77a4b",
        "    for manifest in manifests:\n        key = manifest_key(manifest)",
        "    from hermes_cli._platform_terminal_policy import trusted_manifests\n"
        "    for manifest in trusted_manifests(manifests):\n"
        "        key = manifest_key(manifest)",
    ),
]


def patch(root=ROOT):
    changes = []
    for relative, digest, before, after in PATCHES:
        path = root / relative
        original = path.read_bytes()
        if hashlib.sha256(original).hexdigest() != digest:
            raise RuntimeError("Pinned terminal source changed; review policy hooks")
        text = original.decode()
        if text.count(before) != 1:
            raise RuntimeError("Native terminal policy integration no longer matches")
        text = text.replace(before, after)
        if relative == "tools/terminal_scope.py":
            before_scope = "    _resolve_scope_cwd_placeholder(scope)\n    return scope"
            if text.count(before_scope) != 1:
                raise RuntimeError("Native profile terminal policy no longer matches")
            text = text.replace(
                before_scope,
                "    from hermes_cli._platform_terminal_policy import load_policy\n"
                '    _apply(load_policy()["terminal"])\n'
                "    _resolve_scope_cwd_placeholder(scope)\n    return scope",
            )
        if relative == "hermes_cli/plugins_discovery.py":
            before_gate = "    lookup_key = manifest_key(manifest)\n    names ="
            if text.count(before_gate) != 1:
                raise RuntimeError("Native terminal plugin gate no longer matches")
            text = text.replace(
                before_gate,
                "    from hermes_cli._platform_terminal_policy "
                "import required_manifest\n"
                "    if required_manifest(manifest):\n"
                '        return ManifestGate("load", enabled=True)\n'
                "    lookup_key = manifest_key(manifest)\n    names =",
            )
        compile(text, str(path), "exec")
        changes.append((path, text))
    shutil.copyfile(
        Path(__file__).with_name("terminal-policy.py"),
        root / "hermes_cli/_platform_terminal_policy.py",
    )
    for path, text in changes:
        path.write_text(text)


if __name__ == "__main__":
    patch()
