"""Expected-owner checks applied after native OIDC signature/claim verification.

The manager mounts the required owner policy in its approved workload template.
User-editable home config and environment values cannot replace this policy.
"""

import json
from pathlib import Path
from urllib.parse import urlsplit

from hermes_cli.dashboard_auth import InvalidCodeError

POLICY_FILE = Path("/run/hermes/owner/context.json")


def load_expected_owner(configured_issuer: str) -> tuple[str, str]:
    try:
        with POLICY_FILE.open("rb") as source:
            raw = source.read(16385)
        policy = json.loads(raw)
        if (
            len(raw) > 16384
            or not isinstance(policy, dict)
            or type(policy.get("schemaVersion")) is not int
            or policy["schemaVersion"] != 1
        ):
            raise ValueError
        issuer, subject = policy["owner"]["issuer"], policy["owner"]["subject"]
    except (OSError, ValueError, TypeError, KeyError):
        raise ValueError("Invalid native expected-owner configuration") from None
    if not isinstance(issuer, str) or not isinstance(subject, str):
        raise ValueError("Invalid native expected-owner configuration")
    parsed = urlsplit(issuer)
    if (
        not issuer
        or not subject
        or issuer != configured_issuer
        or len(issuer) > 2048
        or issuer.strip() != issuer
        or any(ord(char) < 33 for char in issuer)
        or parsed.scheme != "https"
        or not parsed.hostname
        or parsed.username is not None
        or parsed.password is not None
        or parsed.query
        or parsed.fragment
        or len(subject) > 255
        or not all(33 <= ord(char) <= 126 for char in subject)
    ):
        raise ValueError("Invalid native expected-owner configuration")
    return issuer, subject


def verify_expected_owner(claims: dict, owner: tuple[str, str]) -> dict:
    if claims.get("iss") != owner[0] or claims.get("sub") != owner[1]:
        # Do not log/reflect tokens, email or either party's identity.
        raise InvalidCodeError("Native instance owner mismatch")
    return claims
