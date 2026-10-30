"""The gateway's interface for a client that holds a platform session (R5, R6).

Cookies are replayed by hand, one explicit `Cookie` header per request, so the
client cannot quietly drop or repair what the case is about. A session is the
pair `__Host-hermes-platform` and `__Host-hermes-platform-browser`; it is valid
only with both, exactly once each, as issued to one browser.

- `me` names the owner and gives the session's CSRF token, and nothing else;
- a logout needs the exact `Origin` and this session's token, and a refusal ends
  nothing on the server (the replayed cookies still work afterwards);
- duplicated, missing, tampered and cross-browser cookies are refused;
- both logouts end the session on the server, so its cookies reach nothing.

A refused logout also leaves the caller's cookies alone: it sets none.
"""

import re

import pytest
import requests

from tests.personal_hermes import native_e2e as scenarios

pytestmark = [pytest.mark.e2e, pytest.mark.cluster, pytest.mark.security]

BASE = f"https://{scenarios.PUBLIC}"
SESSION, PROOF = "__Host-hermes-platform", "__Host-hermes-platform-browser"
HANDLE = re.compile(r"^[a-f0-9]{64}$")
UNAUTHENTICATED = {"error": "invalid_identity_evidence"}
FORBIDDEN = {"error": "access_denied"}


class Held:
    """One signed-in browser's platform cookies, replayed by hand."""

    def __init__(self, platform, name):
        browser = scenarios.signed_in(platform.ca, name, platform.passwords)
        self.platform = platform
        self.cookies = {
            cookie.name: cookie.value
            for cookie in browser.session.cookies
            if cookie.domain.lstrip(".") == scenarios.PUBLIC
        }
        self.csrf = self.call("GET", "/_platform/me").json()["csrfToken"]

    def call(
        self, method, path, cookies=None, names=(SESSION, PROOF), extra="", **headers
    ):
        held = self.cookies if cookies is None else cookies
        cookie = "; ".join(f"{name}={held[name]}" for name in names if name in held)
        return requests.request(
            method,
            f"{BASE}{path}",
            headers={"Cookie": cookie + extra, **headers},
            verify=self.platform.ca,
            timeout=20,
            allow_redirects=False,
        )

    def alive(self):
        return self.call("GET", "/_platform/me").status_code == 200


def test_a_session_reports_its_owner_and_its_csrf_token_and_nothing_else(platform):
    bob, again = Held(platform, "bob"), Held(platform, "bob")
    reply = bob.call("GET", "/_platform/me")
    assert reply.status_code == 200
    assert "no-store" in reply.headers["cache-control"]
    body = reply.json()
    assert set(body) == {"identity", "csrfToken"}
    assert body["identity"] == {
        "issuer": scenarios.ISSUER,
        "subject": scenarios.SUBJECTS["bob"],
    }
    assert HANDLE.match(body["csrfToken"])
    # The token belongs to this session; nothing of the provider or of the
    # cookies is in the body.
    assert body["csrfToken"] != again.csrf
    for secret in bob.cookies.values():
        assert secret not in reply.text


REFUSALS = {
    "no-origin-and-no-token": lambda me, other: {},
    "the-wrong-origin": lambda me, other: {
        "Origin": "https://evil.example",
        "X-CSRF-Token": me.csrf,
    },
    "no-token": lambda me, other: {"Origin": BASE},
    "a-wrong-token": lambda me, other: {"Origin": BASE, "X-CSRF-Token": "0" * 64},
    "a-malformed-token": lambda me, other: {
        "Origin": BASE,
        "X-CSRF-Token": "not-a-handle",
    },
    "another-sessions-token": lambda me, other: {
        "Origin": BASE,
        "X-CSRF-Token": other.csrf,
    },
}


@pytest.mark.parametrize("refusal", REFUSALS)
def test_a_logout_needs_the_exact_origin_and_token_and_a_refusal_ends_nothing(
    platform, refusal
):
    owner, other = Held(platform, "bob"), Held(platform, "carol")
    sent = REFUSALS[refusal](owner, other)
    refused = owner.call("POST", "/_platform/logout", **sent)
    assert refused.status_code == 403 and refused.json() == FORBIDDEN
    assert "set-cookie" not in refused.headers, "a refusal expired cookies"
    assert "no-store" in refused.headers["cache-control"]
    # Nothing ended on the server: the same cookies still work, and both
    # people are as signed in as before.
    assert owner.alive() and other.alive()
    # The exact request does end it.
    ended = owner.call(
        "POST", "/_platform/logout", **{"Origin": BASE, "X-CSRF-Token": owner.csrf}
    )
    assert ended.status_code == 204
    assert not owner.alive()
    assert other.alive(), "another person's session ended"


@pytest.mark.parametrize(
    "fault",
    [
        "duplicated",
        "no-proof",
        "no-session",
        "tampered-session",
        "tampered-proof",
        "another-browsers-proof",
    ],
)
def test_duplicated_missing_tampered_or_foreign_cookies_are_refused(platform, fault):
    owner, other = Held(platform, "bob"), Held(platform, "carol")
    assert owner.alive()
    cookies, names, extra = dict(owner.cookies), (SESSION, PROOF), ""

    def flipped(value):
        return value[:-1] + ("0" if value[-1] != "0" else "1")

    if fault == "duplicated":
        extra = f"; {SESSION}={cookies[SESSION]}"
    elif fault == "no-proof":
        names = (SESSION,)
    elif fault == "no-session":
        names = (PROOF,)
    elif fault == "tampered-session":
        cookies[SESSION] = flipped(cookies[SESSION])
    elif fault == "tampered-proof":
        cookies[PROOF] = flipped(cookies[PROOF])
    else:
        cookies[PROOF] = other.cookies[PROOF]
    for method, path in (("GET", "/_platform/me"), ("GET", "/api/auth/me")):
        reply = owner.call(method, path, cookies=cookies, names=names, extra=extra)
        assert reply.status_code == 401, (path, reply.status_code)
        assert reply.json() == UNAUTHENTICATED
    # The refusals changed nothing: the intact cookies work.
    assert owner.alive()


def test_the_native_logout_needs_the_origin_and_then_ends_the_session(
    platform,
):
    owner = Held(platform, "bob")
    everything = tuple(owner.cookies)
    refused = owner.call(
        "POST", "/auth/logout", names=everything, Origin="https://evil.example"
    )
    assert refused.status_code == 403 and refused.json() == FORBIDDEN
    assert "set-cookie" not in refused.headers, "a refusal expired cookies"
    assert owner.alive(), "a foreign origin ended the session"
    left = owner.call("POST", "/auth/logout", names=everything, Origin=BASE)
    assert left.status_code == 303
    assert left.headers["location"] == "/_platform/signed-out"
    assert not owner.alive(), "the session outlived the logout"
    assert owner.call("GET", "/api/auth/me").status_code == 401


@pytest.mark.parametrize(
    "path", ["/_platform", "/_platform/", "/_platform/nope", "/_platform/a/b"]
)
def test_an_unlisted_platform_path_is_not_found_and_never_reaches_the_instance(
    platform, path
):
    owner = Held(platform, "bob")
    reply = owner.call("GET", path)
    assert reply.status_code == 404, (path, reply.status_code)
    assert reply.json() == {"error": "invalid_request"}
    # Not the instance's page: nothing of its server answered.
    assert "server" not in reply.headers
    assert "no-store" in reply.headers["cache-control"]
    assert owner.alive()
