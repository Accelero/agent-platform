"""Execute against the real pinned native provider and real signed JWTs in Docker."""

import importlib.util
import json
import os
import shutil
import sys
import tempfile
import time
from pathlib import Path

import jwt
from cryptography.hazmat.primitives.asymmetric import rsa

ISSUER = "https://sso.example.org/realms/agents"
SOURCE = Path("/opt/hermes/plugins/dashboard_auth/self_hosted/__init__.py")


def load_file(name, path, package=False):
    spec = importlib.util.spec_from_file_location(
        name, path, submodule_search_locations=[str(path.parent)] if package else None
    )
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def expect_error(error, callback):
    try:
        callback()
    except error:
        return
    raise AssertionError("Foreign/malformed native identity was accepted")


def main():
    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    jwk = jwt.PyJWK.from_json(
        json.dumps(jwt.algorithms.RSAAlgorithm.to_jwk(key.public_key(), as_dict=True))
    )

    class Keys:
        def get_signing_key_from_jwt(self, _token):
            return jwk

    def token(subject, *, email="same@example.org", issuer=ISSUER, signer=key):
        return jwt.encode(
            {
                "iss": issuer,
                "sub": subject,
                "email": email,
                "aud": "native-hermes",
                "iat": int(time.time()),
                "exp": int(time.time()) + 600,
            },
            signer,
            algorithm="RS256",
        )

    with tempfile.TemporaryDirectory(prefix="native-owner-") as temporary:
        target = Path(temporary) / "__init__.py"
        shutil.copyfile(SOURCE, target)
        patcher = load_file(
            "platform_owner_patch", Path("/platform/patch-dashboard-owner.py")
        )
        patcher.patch(target)
        provider_module = load_file(
            "plugins.dashboard_auth.self_hosted", target, package=True
        )
        from hermes_cli.dashboard_auth import (
            InvalidCodeError,
            ProviderError,
            RefreshExpiredError,
        )

        def provider():
            instance = provider_module.SelfHostedOIDCProvider(
                issuer=ISSUER, client_id="native-hermes"
            )
            instance._get_discovery = lambda: {
                "issuer": ISSUER,
                "token_endpoint": "https://sso.example.org/token",
                "jwks_uri": "https://sso.example.org/jwks",
            }
            instance._get_jwks_client = lambda: Keys()
            return instance

        helper = sys.modules["plugins.dashboard_auth.self_hosted._platform_owner"]
        policy = Path(temporary) / "context.json"
        helper.POLICY_FILE = policy
        # Neither an absent marker nor forged environment pins can replace the
        # required mounted owner policy.
        for marker in [None, "", "fixture-instance"]:
            os.environ.pop("HERMES_PERSONAL_INSTANCE_ID", None)
            if marker is not None:
                os.environ["HERMES_PERSONAL_INSTANCE_ID"] = marker
            for issuer, subject in [("", ""), (ISSUER, "owner-a")]:
                os.environ["HERMES_DASHBOARD_EXPECTED_ISSUER"] = issuer
                os.environ["HERMES_DASHBOARD_EXPECTED_SUBJECT"] = subject
                expect_error(ValueError, provider)
        os.environ.pop("HERMES_PERSONAL_INSTANCE_ID", None)
        os.environ.pop("HERMES_DASHBOARD_EXPECTED_ISSUER")
        os.environ.pop("HERMES_DASHBOARD_EXPECTED_SUBJECT")
        policy.write_text(
            json.dumps(
                {"schemaVersion": 1, "owner": {"issuer": ISSUER, "subject": "owner-a"}}
            )
        )
        instance = provider()
        own = token("owner-a")
        foreign = token("owner-b")
        assert instance.verify_session(access_token=own).user_id == "owner-a"
        assert instance.verify_session(access_token=foreign) is None
        assert instance.verify_session(access_token=token("Owner-A")) is None
        assert (
            instance.verify_session(
                access_token=token("owner-a", email="renamed@example.org")
            ).user_id
            == "owner-a"
        )
        expect_error(
            ProviderError,
            lambda: instance.verify_session(
                access_token=token("owner-a", issuer="https://foreign.example.org")
            ),
        )
        other_key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        expect_error(
            ProviderError,
            lambda: instance.verify_session(
                access_token=token("owner-a", signer=other_key)
            ),
        )

        for value in [own, foreign]:
            provider_module.exchange_token = lambda *_args, value=value, **_kwargs: (
                value,
                {"refresh_token": "fixture-refresh"},
            )
            if value == own:
                assert (
                    instance.complete_login(
                        code="fixture-code",
                        state="fixture-state",
                        code_verifier="fixture-verifier",
                        redirect_uri="https://hermes.example.org/auth/callback",
                    ).user_id
                    == "owner-a"
                )
                assert (
                    instance.refresh_session(refresh_token="fixture-refresh").user_id
                    == "owner-a"
                )
            else:
                expect_error(
                    InvalidCodeError,
                    lambda: instance.complete_login(
                        code="fixture-code",
                        state="fixture-state",
                        code_verifier="fixture-verifier",
                        redirect_uri="https://hermes.example.org/auth/callback",
                    ),
                )
                expect_error(
                    RefreshExpiredError,
                    lambda: instance.refresh_session(refresh_token="fixture-refresh"),
                )

        # Missing half of the pin and issuer normalization cannot silently enable
        # an unpinned provider.
        for issuer, subject in [
            (ISSUER, ""),
            ("", "owner-a"),
            (ISSUER + "/", "owner-a"),
            (ISSUER, "bad\nsubject"),
        ]:
            policy.write_text(
                json.dumps(
                    {
                        "schemaVersion": 1,
                        "owner": {"issuer": issuer, "subject": subject},
                    }
                )
            )
            expect_error(ValueError, provider)

        # A mutable native .env cannot replace mounted personal ownership policy.
        policy.unlink()
        expect_error(ValueError, provider)
        policy.write_text(
            json.dumps(
                {"schemaVersion": 1, "owner": {"issuer": ISSUER, "subject": "owner-a"}}
            )
        )
        os.environ["HERMES_DASHBOARD_EXPECTED_ISSUER"] = ISSUER
        os.environ["HERMES_DASHBOARD_EXPECTED_SUBJECT"] = "owner-b"
        mounted = provider()
        assert mounted.verify_session(access_token=own).user_id == "owner-a"
        assert mounted.verify_session(access_token=foreign) is None
        for owner in [
            {"issuer": ISSUER, "subject": None},
            {"issuer": None, "subject": "owner-a"},
            {"issuer": "", "subject": ""},
            {"issuer": None, "subject": None},
        ]:
            policy.write_text(json.dumps({"schemaVersion": 1, "owner": owner}))
            expect_error(ValueError, provider)
        for content in ["broken policy", "[]", "{}", "x" * 16385]:
            policy.write_text(content)
            expect_error(ValueError, provider)
        print(
            "PASS: pinned native owner on login, every session check and refresh; "
            "issuer/signature checks; immutable mounted policy; "
            "no email linking; missing owner policy refused"
        )


if __name__ == "__main__":
    main()
