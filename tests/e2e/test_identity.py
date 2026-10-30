"""Signing-key rotation at the real identity provider, on a platform of its own.

The manager API trusts a fetched key set for five minutes and refetches on a
key ID it does not know. Rotation must therefore work without a restart:

- a token signed by the new key is accepted, and a gateway login still works;
- a token signed by the old key keeps working while the old key is published;
- once the provider stops publishing the old key, that token stops working,
  but only after the cache window, and the new token is unaffected.

The realm's access-token lifespan is raised first, so the old token's end is
caused by its key and not by its expiry.
"""

import base64
import json
import secrets
import time

import pytest
import requests

from tests.personal_hermes import native_e2e as scenarios

pytestmark = [
    pytest.mark.e2e,
    pytest.mark.cluster,
    pytest.mark.destructive,
    pytest.mark.slow,
    pytest.mark.security,
]

# The verifier's key-set cache (`KEY_TTL`) and refetch cooldown.
KEY_TTL = 300
COOLDOWN = 5


def key_id(token):
    header = token.split(".")[0]
    header += "=" * (-len(header) % 4)
    return json.loads(base64.urlsafe_b64decode(header))["kid"]


def status_with(platform, token):
    reply = platform.admin.session.get(
        f"https://{scenarios.ADMIN}/api/v1/session",
        headers={"Authorization": f"Bearer {token}"},
        timeout=20,
    )
    return reply.status_code


def fresh_token(platform):
    platform.admin.issued = 0.0  # forces a new sign-in
    platform.admin.headers()
    return platform.admin.token


def signing_keys(directory):
    listing = directory.call("GET", "/keys").json()
    return listing["active"]["RS256"], {
        key["kid"]: key["providerId"]
        for key in listing["keys"]
        if key["algorithm"] == "RS256" and key["type"] == "RSA"
    }


def test_a_rotated_signing_key_is_followed_without_a_restart_and_the_old_one_is_dropped(
    destructive_platform,
):
    platform = destructive_platform
    directory = scenarios.Directory(platform.ca, platform.passwords["directory"])
    directory.call("PUT", "", {"accessTokenLifespan": 3600}, expected=(204,))

    old_kid, _ = signing_keys(directory)
    old_token = fresh_token(platform)
    assert key_id(old_token) == old_kid
    assert status_with(platform, old_token) == 200, "the control, before rotation"

    realm = directory.call("GET", "").json()["id"]
    directory.call(
        "POST",
        "/components",
        {
            "name": "rotated-signing-key",
            "providerId": "rsa-generated",
            "providerType": "org.keycloak.keys.KeyProvider",
            "parentId": realm,
            "config": {
                "priority": ["200"],
                "keySize": ["2048"],
                "enabled": ["true"],
                "active": ["true"],
            },
        },
        expected=(201,),
    )
    new_kid, published = signing_keys(directory)
    assert new_kid != old_kid and {old_kid, new_kid} <= set(published)

    new_token = fresh_token(platform)
    assert key_id(new_token) == new_kid, "the provider now signs with the new key"
    # An unknown key ID refetches the key set, at most once per cooldown.
    scenarios.wait(
        lambda: status_with(platform, new_token) == 200,
        COOLDOWN * 4,
        "the manager never accepted a token of the rotated key",
        interval=1,
    )
    assert status_with(platform, old_token) == 200, "the old key is still published"
    # The gateway verifies the provider's access token with the same key-set
    # rules: an unknown key ID refetches at most once per cooldown, so a login
    # less than the cooldown after the gateway's last fetch is refused once and
    # accepted on the next try.
    scenarios.wait(
        lambda: (
            scenarios.Browser(platform.ca, "bob", platform.passwords["bob"])
            .sign_in()
            .status_code
            == 200
        ),
        COOLDOWN * 4,
        "a gateway login after rotation",
        interval=2,
    )

    # The provider stops publishing the old key.
    directory.call("DELETE", f"/components/{published[old_kid]}", expected=(204,))
    dropped = time.monotonic()
    assert old_kid not in signing_keys(directory)[1]
    # Within the cache window the old key may still verify (stale by design);
    # past it, with a refetch, it must not.
    time.sleep(max(0, dropped + KEY_TTL + COOLDOWN + 15 - time.monotonic()))
    assert status_with(platform, old_token) in (401, 403), (
        "a token of a key the provider no longer publishes is still accepted"
    )
    assert status_with(platform, new_token) == 200
    assert (
        scenarios.Browser(platform.ca, "carol", platform.passwords["carol"])
        .sign_in()
        .status_code
        == 200
    )


def restart_gateway(platform, keyring, new_keys=()):
    """Sets the gateway's session keyring (and adds key files to its Secret),
    then restarts it; the key material is never printed."""
    identifier = platform.identifier
    if new_keys:
        scenarios.kubectl(
            identifier,
            "patch",
            "secret",
            "hermes-gateway",
            "--namespace",
            scenarios.NAMESPACE,
            "--type",
            "merge",
            "--patch",
            json.dumps(
                {
                    "data": {
                        f"session-key-{name}": base64.b64encode(
                            secrets.token_bytes(32)
                        ).decode()
                        for name in new_keys
                    }
                }
            ),
            quiet=True,
        )
    scenarios.kubectl(
        identifier,
        "patch",
        "configmap",
        "hermes-gateway",
        "--namespace",
        scenarios.NAMESPACE,
        "--type",
        "merge",
        "--patch",
        json.dumps({"data": {"keyring.json": json.dumps(keyring)}}),
    )
    scenarios.kubectl(
        identifier,
        "rollout",
        "restart",
        "deployment/hermes-gateway",
        "--namespace",
        scenarios.NAMESPACE,
    )
    scenarios.kubectl(
        identifier,
        "rollout",
        "status",
        "deployment/hermes-gateway",
        "--namespace",
        scenarios.NAMESPACE,
        "--timeout=180s",
        timeout=200,
    )
    # The rollout is done before the node port reaches the new Pod.
    scenarios.wait(
        lambda: (
            requests.get(
                f"https://{scenarios.PUBLIC}/_platform/healthz",
                verify=platform.ca,
                timeout=5,
            ).status_code
            == 200
        ),
        60,
        "the restarted gateway is unreachable",
        interval=1,
    )


def keyring(active, *ids):
    return {
        "activeKeyId": active,
        "keyFiles": {i: f"/run/hermes/secrets/session-key-{i}" for i in ids},
    }


def test_rotating_the_gateway_session_key_keeps_old_sessions_until_it_is_dropped(
    destructive_platform,
):
    """Sessions are sealed in PostgreSQL under the gateway's active session
    key. A restart with the same ring keeps them; a ring that adds a new active
    key keeps the old sessions readable; dropping the old key ends exactly the
    sessions sealed under it, and people sign in again."""
    platform = destructive_platform
    ca, passwords = platform.ca, platform.passwords
    old = scenarios.signed_in(ca, "bob", passwords)
    assert old.get("/_platform/me").status_code == 200

    # The same ring across a restart: the session survives.
    restart_gateway(platform, keyring("g1", "g1"))
    assert old.get("/_platform/me").status_code == 200, "a restart ended the session"

    # A new active key beside the old: old sessions read, new ones sealed anew.
    restart_gateway(platform, keyring("g2", "g1", "g2"), new_keys=("g2",))
    assert old.get("/_platform/me").status_code == 200, "rotation ended the session"
    fresh = scenarios.signed_in(ca, "carol", passwords)
    assert fresh.get("/_platform/me").status_code == 200

    # The old key dropped: only the sessions sealed under it end.
    restart_gateway(platform, keyring("g2", "g2"))
    assert old.get("/_platform/me").status_code == 401, "the dropped key still reads"
    assert fresh.get("/_platform/me").status_code == 200, "a session of the new key"
    again = scenarios.signed_in(ca, "bob", passwords)
    assert again.get("/_platform/me").status_code == 200


def test_a_session_whose_provider_login_expired_is_refused_and_an_active_one_is_not(
    destructive_platform,
):
    """The gateway keeps a session alive by refreshing it at the provider, at
    most every 60 s while it is used and on demand when it has been idle. A
    session whose provider login has lapsed cannot be refreshed, and what cannot
    be refreshed is not served: the gateway refuses it and the session ends.

    The realm's access-token lifespan is raised first, so that the refusal is
    caused by the lapsed login and not by an expired access token."""
    platform = destructive_platform
    ca, passwords = platform.ca, platform.passwords
    directory = scenarios.Directory(ca, passwords["directory"])
    # The realm is shared with the cases that follow: put back what is changed.
    realm = directory.call("GET", "").json()
    original = {
        key: realm[key] for key in ("accessTokenLifespan", "ssoSessionIdleTimeout")
    }
    try:
        directory.call("PUT", "", {"accessTokenLifespan": 3600}, expected=(204,))

        # Control: with the provider's default idle limit, a session left alone for
        # longer than the gateway trusts its evidence is refreshed on demand.
        unused = scenarios.signed_in(ca, "bob", passwords)
        time.sleep(150)
        assert unused.get("/_platform/me").status_code == 200, "an idle session lapsed"

        # A provider login that lapses after 90 s without use.
        directory.call("PUT", "", {"ssoSessionIdleTimeout": 90}, expected=(204,))
        quiet = scenarios.signed_in(ca, "carol", passwords)
        active = scenarios.signed_in(ca, "bob", passwords)
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            reply = active.get("/api/auth/me")
            assert reply.status_code == 200, (
                f"an active session lapsed: {reply.status_code}"
            )
            time.sleep(20)
        assert quiet.get("/_platform/me").status_code == 401, (
            "a lapsed login was served"
        )
        assert quiet.get("/api/auth/me").status_code == 401
        assert active.get("/api/auth/me").status_code == 200
    finally:
        directory.call("PUT", "", original, expected=(204,))
