"""Validate published manager contracts before generating presentation clients."""

from pathlib import Path

import pytest
import yaml
from openapi_spec_validator import validate

ROOT = Path(__file__).resolve().parents[1]
CONTRACTS = ROOT / "contracts/hermes-manager"


@pytest.mark.parametrize("name", ["openapi.yaml", "router.openapi.yaml"])
def test_manager_contract_is_valid_openapi_31(name):
    path = CONTRACTS / name
    validate(yaml.safe_load(path.read_text()), base_uri=path.as_uri())


def test_assignment_contract_has_no_ownership_transfer_or_arbitrary_workloads():
    document = yaml.safe_load((CONTRACTS / "openapi.yaml").read_text())
    schema = document["components"]["schemas"]["CreateAssignment"]
    assert schema["additionalProperties"] is False
    assert set(schema["properties"]) == {
        "identity",
        "access",
        "templateId",
        "templateVersion",
    }
    patch = document["paths"]["/api/v1/assignments/{id}"]["patch"]
    schema = patch["requestBody"]["content"]["application/json"]["schema"]
    assert schema["additionalProperties"] is False
    assert set(schema["properties"]) == {"access"}
    assert {"409", "412", "422", "503"} <= patch["responses"].keys()
