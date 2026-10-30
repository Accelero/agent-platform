"""Validate published manager contracts before generating presentation clients."""

import json
import subprocess
from pathlib import Path

import pytest
import yaml
from openapi_spec_validator import validate

pytestmark = pytest.mark.unit

ROOT = Path(__file__).resolve().parents[2]
CONTRACTS = ROOT / "contracts/hermes-manager"


def test_all_tracked_dashboard_files_are_in_manager_build_context():
    ui = Path("services/hermes-manager/ui")
    tracked = (
        subprocess.run(
            [
                "git",
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
                "--",
                ui.as_posix(),
            ],
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


def test_all_manager_crate_sources_are_in_manager_build_context():
    crates = Path("services/hermes-manager/crates")
    tracked = (
        subprocess.run(
            [
                "git",
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
                "--",
                crates.as_posix(),
            ],
            cwd=ROOT,
            capture_output=True,
            check=True,
            timeout=10,
        )
        .stdout.decode()
        .split("\0")
    )
    built = {"src", "migrations", "Cargo.toml", "build.rs"}
    expected = {
        name
        for name in tracked
        if name
        and (ROOT / name).exists()
        and Path(name).relative_to(crates).parts[1] in built
    }
    policy = json.loads((ROOT / "release-policy.json").read_text())
    for section in ("contexts", "development_contexts"):
        included = set(policy[section]["hermes-manager"]["files"])
        missing = sorted(expected - included)
        assert not missing, f"Crate sources missing from {section}: {missing}"


@pytest.mark.parametrize(
    "name",
    [
        "openapi.yaml",
        "router.openapi.yaml",
        "execution.openapi.yaml",
        "gateway.openapi.yaml",
    ],
)
def test_manager_contract_is_valid_openapi_31(name):
    path = CONTRACTS / name
    validate(yaml.safe_load(path.read_text()), base_uri=path.as_uri())


def test_assignments_carry_no_workload_specifications():
    document = yaml.safe_load((CONTRACTS / "openapi.yaml").read_text())
    schemas = document["components"]["schemas"]
    for name in ("CreateAssignment", "AssignmentSpec", "AssignmentChanges"):
        assert schemas[name]["additionalProperties"] is False, name
    assert set(schemas["CreateAssignment"]["properties"]) == {"identity", "spec"}
    allowed = {"access", "template", "volumes", "terminal", "adminConfig", "secrets"}
    assert set(schemas["AssignmentChanges"]["properties"]) == allowed
    assert set(schemas["AssignmentSpec"]["properties"]) == allowed
    patch = document["paths"]["/api/v1/assignments/{assignmentId}"]["patch"]
    assert {"409", "412", "422", "503"} <= patch["responses"].keys()
    write = schemas["SecretWrite"]["properties"]["value"]
    assert write.get("writeOnly") is True
    for name, schema in schemas.items():
        if name not in ("SecretWrite",):
            assert "value" not in schema.get("properties", {}), name


def test_resolver_trusts_only_the_gateway_certificate():
    document = yaml.safe_load((CONTRACTS / "router.openapi.yaml").read_text())
    assert document["security"] == [{"GatewayMTLS": []}]
    assert set(document["components"]["securitySchemes"]) == {"GatewayMTLS"}
    operation = document["paths"]["/internal/v1/routes/resolve"]["post"]
    assert set(operation["responses"]) == {"200", "202", "403", "422", "503"}
    request = document["components"]["schemas"]["ResolveRequest"]
    assert set(request["properties"]) == {"identity"}
    assert request["additionalProperties"] is False


def test_the_gateway_owns_only_platform_paths_and_native_logout():
    document = yaml.safe_load((CONTRACTS / "gateway.openapi.yaml").read_text())
    paths = set(document["paths"])
    assert {path for path in paths if not path.startswith("/_platform/")} == {
        "/auth/logout"
    }
    assert set(document["paths"]["/auth/logout"]) == {"post"}


def test_the_access_matrix_is_generated_from_every_declared_operation():
    from tests.support.contract import concrete, operations

    declared = operations()
    public = {operation for operation, _, _, secured in declared if not secured}
    assert len(declared) >= 45, f"the contract declares {len(declared)} operations"
    assert len({operation for operation, *_ in declared}) == len(declared)
    assert public == {"beginAdministratorLogin", "completeAdministratorLogin"}
    secured = {operation for operation, _, _, secured in declared if secured}
    assert {
        "getSession",
        "writeSecret",
        "deleteSecret",
        "grantAdministrator",
    } <= secured
    assert "{" not in "".join(concrete(path) for _, _, path, _ in declared)
