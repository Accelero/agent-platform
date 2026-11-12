"""Validate the recovery inventory and decrypted payload independently of storage."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path


def descriptor() -> dict:
    path = Path(__file__).with_name("contract.json")
    if not path.exists():
        path = (
            Path(__file__).resolve().parents[3]
            / "charts/agent-platform/files/recovery.json"
        )
    return json.loads(path.read_text())


def digest(value: object) -> str:
    return hashlib.sha256(
        json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()


def resolve(config: dict) -> dict:
    expected = descriptor()
    if config.get("recovery") != expected:
        raise ValueError(
            "Unsupported recovery contract or altered application inventory."
        )
    if config.get("database_major") != expected["databaseMajor"] or set(
        config["datasets"]
    ) != set(expected["datasets"]):
        raise ValueError("Recovery database or dataset bindings are incompatible.")
    namespaces = {
        "application": config["namespace"],
        "execution": config["execution_namespace"],
    }
    required = [
        {
            "namespace": namespaces[item["scope"]],
            "name": item["name"],
            "required": item["required"],
        }
        for item in expected["secrets"]
    ]
    if (
        any(item not in config["secrets"] for item in required)
        or config["writers"] != expected["writers"]
        or config["backup_selections"] != expected["selections"]
    ):
        raise ValueError("Required application inventory was changed or omitted.")
    keys = [(item["namespace"], item["name"]) for item in config["secrets"]]
    if len(set(keys)) != len(keys):
        raise ValueError("Duplicate recovery Secret binding.")
    inventory = {
        key: config[key]
        for key in (
            "recovery",
            "datasets",
            "writers",
            "secrets",
            "backup_selections",
            "namespace",
            "execution_namespace",
        )
    }
    if config.get("inventory_sha256") != digest(inventory):
        raise ValueError("Resolved recovery inventory digest is invalid.")
    return config


def validate_payload(config: dict, record: dict, checkpoint: Path) -> dict:
    # Lazy import avoids a cycle and keeps descriptor validation usable offline.
    from platform_recovery.runtime import checkpoint_datasets, inventory, link_roots

    resolve(config)
    for name in ("manifest.json", "database.dump", "secrets.json"):
        path = checkpoint / name
        if not path.is_file() or path.is_symlink():
            raise ValueError("Recovery payload files must be ordinary files.")
    raw = (checkpoint / "manifest.json").read_bytes()
    manifest = json.loads(raw)
    names = checkpoint_datasets(config, manifest)
    if manifest.get("schema") == 3:
        if (
            manifest.get("recovery_api") != 1
            or manifest.get("platform_version")
            not in descriptor()["compatibleReleases"]
            or manifest.get("inventory_sha256") != config["inventory_sha256"]
        ):
            raise ValueError("Unsupported application recovery version.")
    if (
        manifest.get("installation") != config["installation"]
        or manifest.get("database_major") != config["database_major"]
    ):
        raise ValueError(
            "The encrypted recovery manifest is incompatible or incomplete."
        )
    bound = ("namespace_uid", "database_uid", "configuration_sha256", "final")
    if any(
        key not in record or key not in manifest or record[key] != manifest[key]
        for key in bound
    ):
        raise ValueError("Completion metadata does not match the encrypted checkpoint.")
    if hashlib.sha256(raw).hexdigest() != record["manifest_sha256"]:
        raise ValueError("The encrypted manifest does not match the completion record.")
    for name, expected in manifest["datasets"].items():
        root = checkpoint / "datasets" / name
        if (
            not root.is_dir()
            or root.is_symlink()
            or inventory(root, link_roots(checkpoint / "datasets", names)) != expected
        ):
            raise ValueError(
                "A recovered durable dataset failed checksum verification."
            )
    for name, field in (
        ("database.dump", "database_sha256"),
        ("secrets.json", "secrets_sha256"),
    ):
        with (checkpoint / name).open("rb") as data:
            if hashlib.file_digest(data, "sha256").hexdigest() != manifest[field]:
                raise ValueError("Recovered payload checksum verification failed.")
    values = json.loads((checkpoint / "secrets.json").read_text())
    allowed = {(item["namespace"], item["name"]) for item in config["secrets"]}
    required = {
        (item["namespace"], item["name"])
        for item in config["secrets"]
        if item["required"]
    }
    actual = {(item["namespace"], item["name"]) for item in values}
    if not required <= actual or not actual <= allowed or len(actual) != len(values):
        raise ValueError("Recovered credential inventory is incomplete or foreign.")
    return manifest
