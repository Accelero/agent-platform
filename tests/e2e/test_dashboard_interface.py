"""The dashboard's interface, with no session and with one (R1, R2, R7).

`contracts/hermes-manager/openapi.yaml` declares the administration host's
login, callback, logout and session; the rest follows from the dashboard rules.
Each unauthenticated row below is one request and its exact outcome: status,
body or redirect, the headers every answer carries (`no-store`, `nosniff`, no
referrer), the cookies it sets, and that nothing the client sent is reflected.

The signed-in cases replay the administrator's cookies by hand, one explicit
`Cookie` header per request, so a client cannot quietly repair what the case is
about:

- a mutation made with a browser session needs the exact `Origin` and this
  session's CSRF token, and a refusal changes nothing (no effect, no cookie);
- a logout is refused the same way and ends nothing; the exact one ends the
  session on the server and expires the cookies;
- duplicated, missing, tampered and cross-browser cookies are refused;
- a request that presents both a bearer token and a session is ambiguous and is
  refused, whichever of the two would have been enough.
"""

import re
import secrets
from urllib.parse import quote

import pytest
import requests

from tests.personal_hermes import native_e2e as scenarios

pytestmark = [pytest.mark.e2e, pytest.mark.cluster, pytest.mark.security]

BASE = f"https://{scenarios.ADMIN}"
SESSION, PROOF = "__Host-hermes-admin", "__Host-hermes-admin-browser"
HTML = {"Accept": "text/html"}
ORIGIN = {"Origin": BASE}
HANDLE = re.compile(r"^[a-f0-9]{64}$")
UNAUTHENTICATED = {"error": "invalid_identity_evidence"}
FORBIDDEN = {"error": "access_denied"}
INVALID = {"error": "invalid_request"}


def refusal(status, body):
    return {"status": status, "body": body, "cookies": []}


def row(name, method, path, expect, headers=None, **request):
    return pytest.param(method, path, headers or {}, request, expect, id=name)


CASES = [
    # --- GET /auth/login
    row("login", "GET", "/auth/login", {"status": 303, "login": True}),
    row(
        "login-with-a-local-return-path",
        "GET",
        "/auth/login?returnTo=%2Ftemplates",
        {"status": 303, "login": True},
    ),
    *(
        row(
            f"login-refuses-{name}",
            "GET",
            f"/auth/login?returnTo={quote(value, safe='')}",
            refusal(422, INVALID),
        )
        for name, value in {
            "a-protocol-relative-url": "//evil.example/x",
            "an-absolute-url": "https://evil.example/",
            "an-auth-path": "/auth/login",
            "an-api-path": "/api/v1/session",
            "a-platform-path": "/_platform/me",
            "a-path-over-2048-bytes": "/" + "a" * 2100,
        }.items()
    ),
    # --- GET /auth/oidc/callback
    row(
        "callback-without-parameters",
        "GET",
        "/auth/oidc/callback",
        refusal(401, UNAUTHENTICATED),
    ),
    row(
        "callback-with-an-unknown-state-and-code",
        "GET",
        "/auth/oidc/callback?state=unknown-state&code=unknown-code",
        refusal(401, UNAUTHENTICATED),
    ),
    row(
        "callback-with-a-provider-error",
        "GET",
        "/auth/oidc/callback?state=unknown-state&error=access_denied"
        "&error_description=unknown-description",
        refusal(401, UNAUTHENTICATED),
    ),
    # --- POST /auth/logout
    row("logout-without-an-origin", "POST", "/auth/logout", refusal(403, FORBIDDEN)),
    row(
        "logout-from-another-origin",
        "POST",
        "/auth/logout",
        refusal(403, FORBIDDEN),
        {"Origin": "https://evil.example"},
    ),
    row(
        "logout-from-the-origin-without-a-session",
        "POST",
        "/auth/logout",
        {"status": 401, "body": UNAUTHENTICATED},
        ORIGIN,
    ),
    # --- pages with no policy of their own
    row(
        "signed-out",
        "GET",
        "/auth/signed-out",
        {"status": 200, "page": True, "cookies": []},
    ),
    row("healthz", "GET", "/healthz", {"status": 200, "text": "ok", "cookies": []}),
    # --- everything else needs a session
    row("session", "GET", "/api/v1/session", refusal(401, UNAUTHENTICATED)),
    *(
        row(
            f"a-{method.lower()}-request-to-the-api",
            method,
            "/api/v1/assignments",
            refusal(401, UNAUTHENTICATED),
        )
        for method in ("GET", "POST")
    ),
    row(
        "a-page-load-is-sent-to-sign-in",
        "GET",
        "/",
        {"status": 303, "location": "/auth/login?returnTo=%2F", "cookies": []},
        HTML,
    ),
    row(
        "a-page-load-keeps-its-path-and-drops-its-query",
        "GET",
        "/templates?x=1",
        {"status": 303, "location": "/auth/login?returnTo=%2Ftemplates", "cookies": []},
        HTML,
    ),
    row(
        "a-head-page-load-is-sent-to-sign-in",
        "HEAD",
        "/",
        {"status": 303, "location": "/auth/login?returnTo=%2F", "cookies": []},
        HTML,
    ),
    row(
        "an-asset-needs-a-session",
        "GET",
        "/assets/nothing.js",
        refusal(401, UNAUTHENTICATED),
    ),
    row(
        "an-unknown-path-is-json", "GET", "/nope", refusal(404, {"error": "not_found"})
    ),
    row(
        "an-unknown-api-path-is-json",
        "GET",
        "/api/v1/nope",
        refusal(404, {"error": "not_found"}),
    ),
]


def cookies_of(reply):
    return [
        (header.split("=", 1)[0], header)
        for header in reply.raw.headers.getlist("Set-Cookie")
    ]


@pytest.mark.parametrize("method, path, headers, request_args, expect", CASES)
def test_the_dashboard_answers_a_client_without_a_session_as_declared(
    platform, method, path, headers, request_args, expect
):
    reply = requests.request(
        method,
        f"{BASE}{path}",
        headers=headers,
        verify=platform.ca,
        timeout=20,
        allow_redirects=False,
        **request_args,
    )
    assert reply.status_code == expect["status"], (reply.status_code, reply.text[:200])
    assert reply.headers["x-content-type-options"] == "nosniff"
    assert reply.headers["referrer-policy"] == "no-referrer"
    assert "no-store" in reply.headers["cache-control"]
    if "body" in expect:
        assert reply.json() == expect["body"]
    if "text" in expect:
        assert reply.text == expect["text"]
    if "location" in expect:
        assert reply.headers["location"] == expect["location"]
    if expect.get("page"):
        assert "default-src 'none'" in reply.headers["content-security-policy"]
        assert "<a " in reply.text
    if expect.get("login"):
        assert reply.headers["location"].startswith(
            scenarios.ISSUER + "/protocol/openid-connect/auth?"
        )
        query = dict(
            part.split("=", 1)
            for part in reply.headers["location"].split("?", 1)[1].split("&")
        )
        assert query["client_id"] == "manager-admin"
        assert query["code_challenge_method"] == "S256" and query["scope"] == "openid"
        assert query["redirect_uri"] == quote(f"{BASE}/auth/oidc/callback", safe="")
        proof = [h for n, h in cookies_of(reply) if n == PROOF]
        assert len(proof) == 1, cookies_of(reply)
        assert "Secure" in proof[0] and "HttpOnly" in proof[0]
        assert "SameSite=Lax" in proof[0] and "Path=/" in proof[0]
        assert "Domain" not in proof[0]
    if "cookies" in expect:
        assert [name for name, _ in cookies_of(reply)] == expect["cookies"]
    if reply.status_code >= 400:
        for sent in (
            "unknown-state",
            "unknown-code",
            "unknown-description",
            "evil.example",
        ):
            assert sent not in reply.text


class Held:
    """One administrator browser's cookies, replayed by hand."""

    def __init__(self, platform, name="alice"):
        self.platform = platform
        session = scenarios.resilient(requests.Session())
        session.verify = platform.ca
        page = session.get(f"{BASE}/", headers=HTML, timeout=30)
        assert scenarios.KEYCLOAK in page.url, "not sent to the provider"
        landed = scenarios.keycloak_login(session, page, name, platform.passwords[name])
        assert landed.status_code == 200, landed.status_code
        self.cookies = {
            cookie.name: cookie.value
            for cookie in session.cookies
            if cookie.domain.lstrip(".") == scenarios.ADMIN
        }
        assert set(self.cookies) == {SESSION, PROOF}
        self.csrf = self.call("GET", "/api/v1/session").json()["csrfToken"]

    def call(
        self,
        method,
        path,
        cookies=None,
        names=(SESSION, PROOF),
        extra="",
        body=None,
        **headers,
    ):
        held = self.cookies if cookies is None else cookies
        cookie = "; ".join(f"{name}={held[name]}" for name in names if name in held)
        return requests.request(
            method,
            f"{BASE}{path}",
            headers={"Cookie": cookie + extra, **headers},
            json=body,
            verify=self.platform.ca,
            timeout=20,
            allow_redirects=False,
        )

    def alive(self):
        return self.call("GET", "/api/v1/session").status_code == 200


def test_a_session_reports_the_administrator_and_a_csrf_token_and_nothing_else(
    platform,
):
    one, two = Held(platform), Held(platform)
    reply = one.call("GET", "/api/v1/session")
    assert reply.status_code == 200 and "no-store" in reply.headers["cache-control"]
    body = reply.json()
    assert set(body) == {"actor", "permittedActions", "csrfToken"}
    assert body["actor"] == {
        "issuer": scenarios.ISSUER,
        "subject": scenarios.SUBJECTS["alice"],
    }
    assert sorted(body["permittedActions"]) == ["administer", "administrator.manage"]
    assert HANDLE.match(body["csrfToken"])
    assert body["csrfToken"] != two.csrf, "two browsers share a CSRF token"
    for secret in one.cookies.values():
        assert secret not in reply.text
    assert one.alive() and two.alive(), "a second sign-in ended the first session"


REFUSALS = {
    "cookies-only": lambda me, other: {},
    "the-wrong-origin": lambda me, other: {
        "Origin": "https://evil.example",
        "X-CSRF-Token": me.csrf,
    },
    "no-token": lambda me, other: {"Origin": BASE},
    "a-wrong-token": lambda me, other: {"Origin": BASE, "X-CSRF-Token": "0" * 64},
    "another-sessions-token": lambda me, other: {
        "Origin": BASE,
        "X-CSRF-Token": other.csrf,
    },
}


@pytest.mark.parametrize("refusal", REFUSALS)
def test_a_browser_mutation_needs_the_exact_origin_and_this_sessions_token(
    platform, refusal
):
    me, other = Held(platform), Held(platform)
    name = f"csrf-{secrets.token_hex(4)}"
    value = {"kind": "env", "value": f"canary-{secrets.token_hex(12)}"}
    sent = REFUSALS[refusal](me, other)
    refused = me.call(
        "PUT",
        f"/api/v1/secrets/{name}",
        body=value,
        **{"Idempotency-Key": secrets.token_hex(8), **sent},
    )
    assert refused.status_code == 403 and refused.json() == FORBIDDEN
    assert "set-cookie" not in refused.headers
    names = {
        s["name"]
        for s in platform.admin.call("GET", "/api/v1/secrets?limit=100").json()["items"]
    }
    assert name not in names, "a refused mutation was applied"
    assert me.alive(), "a refusal ended the session"
    # The exact request is applied, and is the only difference.
    done = me.call(
        "PUT",
        f"/api/v1/secrets/{name}",
        body=value,
        **{
            "Idempotency-Key": secrets.token_hex(8),
            "Origin": BASE,
            "X-CSRF-Token": me.csrf,
        },
    )
    assert done.status_code == 200, done.text
    platform.admin.call("DELETE", f"/api/v1/secrets/{name}", etag='"1"')


@pytest.mark.parametrize("refusal", REFUSALS)
def test_a_logout_needs_the_exact_origin_and_token_and_a_refusal_ends_nothing(
    platform, refusal
):
    me, other = Held(platform), Held(platform)
    sent = REFUSALS[refusal](me, other)
    refused = me.call("POST", "/auth/logout", **sent)
    assert refused.status_code == 403 and refused.json() == FORBIDDEN
    assert "set-cookie" not in refused.headers, "a refusal expired cookies"
    assert me.alive() and other.alive()
    ended = me.call("POST", "/auth/logout", **{"Origin": BASE, "X-CSRF-Token": me.csrf})
    assert ended.status_code == 204
    expired = [h for _, h in cookies_of(ended)]
    for cookie in (SESSION, PROOF):
        assert any(h.startswith(f"{cookie}=;") and "Max-Age=0" in h for h in expired), (
            cookie
        )
    assert not me.alive(), "the session outlived its logout"
    assert other.alive(), "another browser's session ended"


FAULTS = [
    "duplicated",
    "no-proof",
    "no-session",
    "tampered-session",
    "tampered-proof",
    "another-browsers-proof",
]


@pytest.mark.parametrize("fault", FAULTS)
def test_duplicated_missing_tampered_or_foreign_cookies_are_refused(platform, fault):
    me, other = Held(platform), Held(platform)
    cookies, names, extra = dict(me.cookies), (SESSION, PROOF), ""

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
    for path in ("/api/v1/session", "/api/v1/assignments?limit=1"):
        reply = me.call("GET", path, cookies=cookies, names=names, extra=extra)
        assert reply.status_code == 401 and reply.json() == UNAUTHENTICATED, path
    assert me.alive(), "the refusals changed the intact session"


def test_a_request_with_both_a_bearer_token_and_a_session_is_refused(platform):
    me = Held(platform)
    bearer = {"Authorization": platform.admin.headers()["Authorization"]}
    for headers in (
        {},
        {"Origin": BASE, "X-CSRF-Token": me.csrf},
    ):
        reply = me.call("GET", "/api/v1/session", **bearer, **headers)
        assert reply.status_code == 401 and reply.json() == UNAUTHENTICATED, headers
    # Each credential alone is enough for the same read.
    assert me.alive()
    alone = requests.get(
        f"{BASE}/api/v1/session", headers=bearer, verify=platform.ca, timeout=20
    )
    assert alone.status_code == 200
