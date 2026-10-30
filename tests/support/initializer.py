"""Loads the image's native home initializer and writes its mounted inputs."""

import importlib.util
import json
from pathlib import Path

SOURCE = Path(__file__).resolve().parents[2] / "runtimes/hermes/personal-init.py"
REFUSAL = "Native initialization unavailable; private details suppressed."

CONTEXT = {
    "schemaVersion": 1,
    "installationId": "00000000-0000-4000-8000-000000000001",
    "instanceId": "00000000-0000-4000-8000-000000000002",
    "materialId": "00000000-0000-4000-8000-000000000003",
    "owner": {
        "issuer": "https://sso.example.org/realms/agents",
        "subject": "owner-a",
    },
    "template": {"id": "personal-default", "version": 1},
}
CONFIG = {"terminal": {"backend": "managed_execution"}}
KEY = "2a" * 32


def load():
    spec = importlib.util.spec_from_file_location("personal_init", SOURCE)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def write_inputs(directory, *, context=CONTEXT, config=CONFIG, key=KEY):
    """The three mounted inputs; a `bytes` value is written verbatim, anything
    else as JSON and `None` leaves the file absent. Returns the paths."""
    paths = {
        "context": directory / "context.json",
        "config": directory / "native-config.json",
        "key": directory / "init-key",
    }
    for name, value in (("context", context), ("config", config), ("key", key)):
        if value is None:
            continue
        if isinstance(value, str):
            value = value.encode()
        elif not isinstance(value, bytes):
            value = json.dumps(value).encode()
        paths[name].write_bytes(value)
        paths[name].chmod(0o600)
    return paths
