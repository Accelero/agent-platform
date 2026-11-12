"""Reject damaged payloads and inconsistent inventories across supported schemas."""

import hashlib
import json
from copy import deepcopy
from pathlib import Path

import pytest
from platform_recovery import runtime
from platform_recovery.contract import (
    descriptor,
    digest,
    logical_inventory,
    resolve,
    validate_payload,
)

pytestmark = pytest.mark.unit


@pytest.fixture
def config() -> dict:
    recovery = descriptor()
    namespaces = {"application": "agents", "execution": "agent-execution"}
    value = {
        "recovery": recovery,
        "datasets": dict.fromkeys(recovery["datasets"], "1Gi"),
        "database_major": 17,
        "installation": "fixture",
        "namespace": "agents",
        "execution_namespace": "agent-execution",
        "writers": recovery["writers"],
        "backup_selections": recovery["selections"],
        "secrets": [
            {
                "namespace": namespaces[x["scope"]],
                "name": x["name"],
                "required": x["required"],
            }
            for x in recovery["secrets"]
        ],
    }
    value["inventory_sha256"] = digest(logical_inventory(value))
    return value


def make_payload(config, tmp_path, schema):
    names = config["datasets"] if schema == 1 else config["backup_selections"]["1"]
    manifest = {
        "schema": schema,
        "selection": 1,
        "installation": "fixture",
        "database_major": 17,
        "namespace_uid": "source-ns",
        "database_uid": "source-db",
        "configuration_sha256": "source-config",
        "final": True,
        "recovery_api": 1,
        "inventory_sha256": config["inventory_sha256"],
        "inventory": logical_inventory(config),
        "platform_version": "0.8.0",
        "datasets": {},
    }
    for name in names:
        root = tmp_path / "datasets" / name
        root.mkdir(parents=True)
        (root / "work").write_text("retained work")
        manifest["datasets"][name] = runtime.inventory(root)
    secrets = [
        {
            "namespace": x["namespace"],
            "name": x["name"],
            "data": {"fixture": "Zml4dHVyZQ=="},
            "type": "Opaque",
        }
        for x in config["secrets"]
    ]
    (tmp_path / "secrets.json").write_text(json.dumps(secrets))
    (tmp_path / "database.dump").write_bytes(b"synthetic-dump")
    for name, field in (
        ("database.dump", "database_sha256"),
        ("secrets.json", "secrets_sha256"),
    ):
        manifest[field] = hashlib.sha256((tmp_path / name).read_bytes()).hexdigest()
    raw = json.dumps(manifest).encode()
    (tmp_path / "manifest.json").write_bytes(raw)
    record = {
        **manifest,
        "snapshot": "a" * 64,
        "manifest_sha256": hashlib.sha256(raw).hexdigest(),
    }
    return manifest, record


@pytest.mark.parametrize("schema", [1, 2, 3])
def test_supported_payload_roundtrip_and_corruption(config, tmp_path, schema):
    manifest, record = make_payload(config, tmp_path, schema)
    assert validate_payload(config, record, tmp_path) == manifest
    with pytest.raises(ValueError, match="metadata"):
        validate_payload(config, {**record, "database_uid": "wrong"}, tmp_path)
    (tmp_path / "datasets/hermes-home/work").write_text("corrupt")
    with pytest.raises(ValueError, match="checksum"):
        validate_payload(config, record, tmp_path)


@pytest.mark.parametrize(
    "field", ["recovery", "writers", "secrets", "backup_selections", "inventory_sha256"]
)
def test_changed_or_missing_inventory_refuses(config, field):
    altered = deepcopy(config)
    altered[field] = {} if field in ("recovery", "backup_selections") else []
    with pytest.raises(ValueError):
        resolve(altered)


def test_new_payload_rejects_old_reader_selection(config):
    with pytest.raises(ValueError, match="incompatible"):
        runtime.checkpoint_datasets(config, {"schema": 99, "datasets": {}})


def test_descriptor_and_release_version_cannot_drift():
    import yaml

    root = Path(__file__).parents[1]
    chart = yaml.safe_load((root / "charts/agent-platform/Chart.yaml").read_text())
    assert descriptor()["platformVersion"] == chart["version"]
    assert descriptor()["platformVersion"] in descriptor()["compatibleReleases"]


@pytest.mark.parametrize("database_uid", ["database-uid", "wrong-uid"])
def test_database_identity_uses_deployment_binding(config, database_uid):
    from unittest.mock import MagicMock, patch

    config.update(
        namespace_uid="namespace-uid",
        database_uid="database-uid",
        database_resource={
            "group": "fixture.io",
            "version": "v1",
            "plural": "databases",
            "name": "external-database",
        },
    )
    core = MagicMock()
    core.read_namespace.return_value.metadata.uid = "namespace-uid"
    custom = MagicMock()
    custom.get_namespaced_custom_object.return_value = {
        "metadata": {"uid": database_uid}
    }
    with (
        patch.object(runtime.kubernetes.config, "load_incluster_config"),
        patch.object(runtime.kube, "CoreV1Api", return_value=core),
        patch.object(runtime.kube, "CustomObjectsApi", return_value=custom),
    ):
        if database_uid == "wrong-uid":
            with pytest.raises(RuntimeError, match="database identity"):
                runtime.Cluster(config)
        else:
            runtime.Cluster(config)
        assert custom.get_namespaced_custom_object.call_args.args == (
            "fixture.io",
            "v1",
            "agents",
            "databases",
            "external-database",
        )


@pytest.mark.parametrize("extra_selection", [False, True])
@pytest.mark.parametrize("change_extension", [False, True])
def test_previous_release_snapshot_is_verified_by_next_reader(
    config, tmp_path, change_extension, extra_selection
):
    from unittest.mock import patch

    from platform_recovery import contract

    config["secrets"].append(
        {"namespace": "tailscale", "name": "old-cert", "required": False}
    )
    config["inventory_sha256"] = digest(logical_inventory(config))
    manifest, record = make_payload(config, tmp_path, 3)
    reader = deepcopy(config)
    reader["recovery"]["platformVersion"] = "0.8.1"
    reader["recovery"]["compatibleReleases"].append("0.8.1")
    reader["recovery"]["payloadSchemas"].append(4)
    if extra_selection:
        reader["recovery"]["selections"]["2"] = list(
            reader["recovery"]["selections"]["1"]
        )
        reader["backup_selections"] = reader["recovery"]["selections"]
    if change_extension:
        reader["secrets"][-1]["name"] = "new-cert"
    reader["inventory_sha256"] = digest(logical_inventory(reader))
    with patch.object(contract, "descriptor", return_value=reader["recovery"]):
        assert validate_payload(reader, record, tmp_path) == manifest
    if not change_extension and not extra_selection:
        assert reader["inventory_sha256"] == config["inventory_sha256"]


def test_obsolete_optional_secret_is_verified_then_skipped_on_restore(config, tmp_path):
    from unittest.mock import MagicMock, patch

    config["secrets"].append(
        {"namespace": "tailscale", "name": "old-cert", "required": False}
    )
    config["inventory_sha256"] = digest(logical_inventory(config))
    manifest, record = make_payload(config, tmp_path / "payload", 3)
    config["secrets"][-1]["name"] = "new-cert"
    config["inventory_sha256"] = digest(logical_inventory(config))
    validate_payload(config, record, tmp_path / "payload")
    datasets = tmp_path / "target"
    for name in config["datasets"]:
        (datasets / name).mkdir(parents=True)
    staging = tmp_path / "staging"
    staging.mkdir()
    config["namespace_uid"] = "target-namespace"
    cluster, repository = MagicMock(), MagicMock()
    cluster.read_record.return_value = None
    repository.pointer.return_value = record
    repository.verify.return_value = (tmp_path / "payload", manifest)
    with (
        patch.object(runtime, "Cluster", return_value=cluster),
        patch.object(runtime, "Repository", return_value=repository),
        patch.object(runtime, "STAGING", staging),
        patch.object(runtime, "DATASETS", datasets),
    ):
        runtime.restore_files(config)
    restored = cluster.restore_secrets.call_args.args[0]
    assert not any(item["name"] == "old-cert" for item in restored)
    assert any(item["name"] == "agent-platform-secrets" for item in restored)
