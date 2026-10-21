"""Personal-instance terminal policy, loaded from read-only image/mounts.

Legacy runtimes keep upstream behavior. Personal instances require the manager's
policy and the image-owned provider regardless of mutable profile settings.
"""

import json
import os
from pathlib import Path

OWNER_CONTEXT = Path("/run/hermes/owner/context.json")
MANAGED_DIR = Path("/run/hermes/execution-ca")
PLUGIN_DIR = Path("/opt/hermes/plugins/managed_execution")
PLUGIN = "managed_execution"


def personal_mode():
    # The read-only owner mount remains authoritative if a profile .env changes
    # the ambient marker. Numeric user/profile names are never authentication.
    return OWNER_CONTEXT.is_file() or bool(
        os.environ.get("HERMES_PERSONAL_INSTANCE_ID")
    )


def load_policy():
    if not personal_mode():
        return None
    try:
        with (MANAGED_DIR / "config.yaml").open("rb") as source:
            content = source.read(16385)
        if len(content) > 16384:
            raise ValueError
        policy = json.loads(content)
        plugins = policy["plugins"]
        enabled, disabled = plugins["enabled"], plugins["disabled"]
        if (
            policy["terminal"]["backend"] != PLUGIN
            or not isinstance(enabled, list)
            or PLUGIN not in enabled
            or not isinstance(disabled, list)
            or PLUGIN in disabled
        ):
            raise ValueError
    except (OSError, ValueError, KeyError, TypeError):
        # Never include configuration, credentials or parser errors in output.
        raise RuntimeError("Personal terminal policy is missing or invalid") from None
    return policy


def managed_directory():
    return MANAGED_DIR if load_policy() is not None else None


def terminal_override(name):
    policy = load_policy()
    if policy is None:
        return None
    from hermes_cli.config import TERMINAL_CONFIG_ENV_MAP, _terminal_env_value

    for key, value in policy["terminal"].items():
        if TERMINAL_CONFIG_ENV_MAP.get(key) == name:
            return _terminal_env_value(value)
    return None


def enforce_backend(name):
    if load_policy() is not None and name != PLUGIN:
        raise ValueError(
            "Personal instances require the managed_execution terminal provider"
        )


def _reserved(manifest):
    return manifest.name == PLUGIN or getattr(manifest, "key", None) == PLUGIN


def _trusted(manifest):
    return (
        _reserved(manifest)
        and manifest.source == "bundled"
        and Path(manifest.path).resolve() == PLUGIN_DIR
    )


def trusted_manifests(manifests):
    if load_policy() is None:
        return manifests
    pinned = [manifest for manifest in manifests if _trusted(manifest)]
    if len(pinned) != 1:
        raise RuntimeError("Required image-owned terminal plugin is unavailable")
    return [manifest for manifest in manifests if not _reserved(manifest)] + pinned


def required_manifest(manifest):
    if load_policy() is None or not _reserved(manifest):
        return False
    if not _trusted(manifest):
        raise RuntimeError("Personal terminal plugin must come from the image")
    return True
