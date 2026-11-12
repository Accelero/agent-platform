"""Call a deployment-owned JSON transport command without importing its code."""

from __future__ import annotations

import json
from pathlib import Path

from platform_recovery.commands import run


class Repository:
    def __init__(self, config: dict) -> None:
        self.config = config

    def request(self, action: str, **values: object) -> object:
        command = self.config.get("transport_command")
        if (
            not isinstance(command, list)
            or not command
            or any(not isinstance(item, str) or not item for item in command)
        ):
            raise ValueError("An explicit trusted transport command is required.")
        return json.loads(
            run(
                command,
                data=json.dumps(
                    {
                        "apiVersion": 1,
                        "config": self.config,
                        "action": action,
                        "values": values,
                    }
                ).encode(),
                timeout=1250,
                cleanup_grace=35,
            )
        )

    def pointer(self) -> dict | None:
        return self.request("pointer")

    def invalidate_cutoff(self) -> None:
        self.request("invalidate_cutoff")

    def initialize_empty(self) -> None:
        self.request("initialize_empty")

    def verify(self, record: dict, target: Path) -> tuple[Path, dict]:
        result = self.request("verify", record=record, target=str(target))
        from platform_recovery.contract import validate_payload

        checkpoint = Path(result["checkpoint"]).resolve(strict=True)
        if not checkpoint.is_relative_to(target.resolve(strict=True)):
            raise ValueError("Transport checkpoint escapes the requested target.")
        manifest = validate_payload(self.config, record, checkpoint)
        return checkpoint, manifest

    def retain(self, keep: str | None) -> None:
        self.request("retain", keep=keep)

    def checkpoint(self, root: Path, manifest: dict, verify: Path) -> dict:
        return self.request(
            "checkpoint", root=str(root), manifest=manifest, verify=str(verify)
        )
