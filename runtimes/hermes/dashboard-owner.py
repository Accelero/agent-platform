"""Expected-owner checks applied after native OIDC signature/claim verification.

The manager supplies these environment values in its approved workload template.
They are immutable deployment policy, never read from user-editable home config.
Leaving both absent preserves the existing singleton provider behavior.
"""

import os
from urllib.parse import urlsplit

from hermes_cli.dashboard_auth import InvalidCodeError


def load_expected_owner(configured_issuer: str) -> tuple[str, str] | None:
    issuer = os.environ.get("HERMES_DASHBOARD_EXPECTED_ISSUER", "")
    subject = os.environ.get("HERMES_DASHBOARD_EXPECTED_SUBJECT", "")
    if not issuer and not subject:
        return None
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


def verify_expected_owner(claims: dict, owner: tuple[str, str] | None) -> dict:
    if owner is not None and (
        claims.get("iss") != owner[0] or claims.get("sub") != owner[1]
    ):
        # Do not log/reflect tokens, email or either party's identity.
        raise InvalidCodeError("Native instance owner mismatch")
    return claims
