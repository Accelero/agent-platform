"""Check responses against contracts/runtime-manager/execution.openapi.yaml.

A small validator for the schema keywords the contract uses (type, enum, required,
properties, additionalProperties, items, minimum, nullable, allOf, $ref).
"""

from pathlib import Path

import yaml

from agent_platform.build import ROOT

CONTRACT = yaml.safe_load(
    (ROOT / "contracts/runtime-manager/execution.openapi.yaml").read_text()
)
TYPES = {
    "object": dict,
    "array": list,
    "string": str,
    "integer": int,
    "number": (int, float),
    "boolean": bool,
}


def _resolve(schema: dict) -> dict:
    while "$ref" in schema:
        node = CONTRACT
        for part in schema["$ref"].removeprefix("#/").split("/"):
            node = node[part]
        schema = node
    return schema


def validate(value, schema: dict, where: str = "body") -> None:
    schema = _resolve(schema)
    if value is None and schema.get("nullable"):
        return
    for part in schema.get("allOf", []):
        validate(value, part, where)
    expected = schema.get("type")
    if expected:
        python = TYPES[expected]
        ok = isinstance(value, python) and not (
            expected in ("integer", "number") and isinstance(value, bool)
        )
        assert ok, f"{where}: {value!r} is not {expected}"
    if "enum" in schema:
        assert value in schema["enum"], f"{where}: {value!r} not in {schema['enum']}"
    if "minimum" in schema:
        assert value >= schema["minimum"], f"{where}: {value!r} below minimum"
    if isinstance(value, dict):
        for name in schema.get("required", []):
            assert name in value, f"{where}: missing {name}"
        properties = schema.get("properties", {})
        if schema.get("additionalProperties") is False:
            extra = set(value) - set(properties)
            assert not extra, f"{where}: unexpected {sorted(extra)}"
        for name, item in value.items():
            if name in properties:
                validate(item, properties[name], f"{where}.{name}")
    if isinstance(value, list) and "items" in schema:
        for index, item in enumerate(value):
            validate(item, schema["items"], f"{where}[{index}]")


def check(method: str, path: str, status: int, body, headers=None) -> None:
    """Assert that a response is one the contract declares for the operation."""
    if path == "/healthz":
        operation = CONTRACT["paths"][path]["get"]
    else:
        operation = CONTRACT["paths"][path][method.lower()]
    responses = operation["responses"]
    assert str(status) in responses, f"{method} {path}: undeclared status {status}"
    response = _resolve(responses[str(status)])
    validate(body, response["content"]["application/json"]["schema"])
    code = body.get("code") if isinstance(body, dict) else None
    if code in ("environment_pending", "environment_draining"):
        assert (headers or {}).get("Retry-After"), f"{code} must carry Retry-After"


def operations() -> list[tuple[str, str]]:
    return [
        (method.upper(), path)
        for path, item in CONTRACT["paths"].items()
        for method in item
    ]


def contract_path() -> Path:
    return ROOT / "contracts/runtime-manager/execution.openapi.yaml"
