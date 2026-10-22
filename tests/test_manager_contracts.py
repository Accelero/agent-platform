"""Validate published manager contracts before generating presentation clients."""

import json
import subprocess
from pathlib import Path

import pytest
import yaml
from openapi_spec_validator import validate

ROOT = Path(__file__).resolve().parents[1]
CONTRACTS = ROOT / "contracts/hermes-manager"


def test_all_tracked_dashboard_files_are_in_manager_build_context():
    ui = Path("services/hermes-manager/ui")
    tracked = (
        subprocess.run(
            ["git", "ls-files", "-z", "--", ui.as_posix()],
            cwd=ROOT,
            capture_output=True,
            check=True,
            timeout=10,
        )
        .stdout.decode()
        .split("\0")
    )
    excluded = {"dist", "test-results", "node_modules"}
    expected = {
        name
        for name in tracked
        if name and Path(name).relative_to(ui).parts[0] not in excluded
    }
    policy = json.loads((ROOT / "release-policy.json").read_text())
    included = set(policy["development_contexts"]["hermes-manager"]["files"])
    missing = sorted(expected - included)
    assert not missing, f"Dashboard files missing from manager build context: {missing}"


@pytest.mark.parametrize(
    "name",
    [
        "openapi.yaml",
        "router.openapi.yaml",
        "router-auth.openapi.yaml",
        "execution.openapi.yaml",
    ],
)
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
