"""The administration contract as data, for cases generated from it."""

from __future__ import annotations

import re
import uuid
from pathlib import Path

import yaml

CONTRACT = Path(__file__).resolve().parents[2] / "contracts/hermes-manager/openapi.yaml"
METHODS = ("get", "post", "put", "patch", "delete")


def operations():
    """(operationId, method, path, secured) for every declared operation. An
    operation is secured unless it declares an empty `security` list."""
    spec = yaml.safe_load(CONTRACT.read_text())
    default = spec.get("security", [])
    return [
        (
            item[method]["operationId"],
            method,
            path,
            bool(item[method].get("security", default)),
        )
        for path, item in spec["paths"].items()
        for method in METHODS
        if method in item
    ]


def concrete(path):
    """The path with every parameter filled by a well-formed value."""
    return re.sub(
        r"\{([^}]+)\}",
        lambda m: str(uuid.uuid4()) if m.group(1).endswith("Id") else "sample",
        path,
    )
