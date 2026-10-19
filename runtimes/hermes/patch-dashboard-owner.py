"""Apply the narrow native owner hook to the reviewed, pinned Hermes source."""

import hashlib
import shutil
import sys
from pathlib import Path

UPSTREAM_SHA256 = "31b46690fb333fffa1524a63a54225ecc6c8731dcbfdfac95ea7647816cdbf5e"
DEFAULT_SOURCE = Path("/opt/hermes/plugins/dashboard_auth/self_hosted/__init__.py")


def patch(source: Path) -> None:
    original = source.read_bytes()
    if hashlib.sha256(original).hexdigest() != UPSTREAM_SHA256:
        raise RuntimeError("Pinned native OIDC source changed; review the owner hook")
    text = original.decode()
    replacements = [
        (
            "from hermes_cli.dashboard_auth import LoginStart, ProviderError, Session",
            "from hermes_cli.dashboard_auth import (\n"
            "    InvalidCodeError, LoginStart, ProviderError, Session)\n"
            "from ._platform_owner import load_expected_owner, verify_expected_owner",
        ),
        (
            '        self._issuer = issuer.rstrip("/")',
            "        self._platform_expected_owner = load_expected_owner(issuer)\n"
            '        self._issuer = issuer.rstrip("/")',
        ),
        (
            "        claims = self._verify_id_token(id_token)",
            "        try:\n"
            "            claims = self._verify_id_token(id_token)\n"
            "        except InvalidCodeError:\n"
            '            raise bad_request_exc("Invalid native identity") from None',
        ),
        (
            "        return verify_jwt(\n"
            "            id_token, self._get_jwks_client(), "
            "algorithms=list(_ALLOWED_ID_TOKEN_ALGS),\n"
            '            audience=self._client_id, issuer=issuer, label="ID token")',
            "        claims = verify_jwt(\n"
            "            id_token, self._get_jwks_client(), "
            "algorithms=list(_ALLOWED_ID_TOKEN_ALGS),\n"
            '            audience=self._client_id, issuer=issuer, label="ID token")\n'
            "        return verify_expected_owner("
            "claims, self._platform_expected_owner)",
        ),
    ]
    for before, after in replacements:
        if text.count(before) != 1:
            raise RuntimeError(
                "Native OIDC owner hook no longer matches reviewed source"
            )
        text = text.replace(before, after)
    compile(text, str(source), "exec")
    shutil.copyfile(
        Path(__file__).with_name("dashboard-owner.py"),
        source.with_name("_platform_owner.py"),
    )
    source.write_text(text)


if __name__ == "__main__":
    patch(Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_SOURCE)
