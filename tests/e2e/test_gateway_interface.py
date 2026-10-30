"""The gateway's public interface, operation by operation, without a session (R5, R6).

`contracts/hermes-manager/gateway.openapi.yaml` declares what a client that has
no platform session may do: begin a login, fail a callback, ask who it is, try to
log out, read the signed-out and liveness pages, and reach the proxied
application. Each row below is one request and its exact outcome: status, error
body or redirect, the headers the contract and the security model require
(`no-store` under `/_platform`, `nosniff`, no referrer), and which cookies the
response sets. Refusals set none and never echo what the client sent.

A logout refused for its Origin or token expires nothing; one with a good Origin
and no session is answered `401` and clears the stale cookies, which its rows do
not pin.
"""

import re
from urllib.parse import quote, urlparse

import pytest
import requests

from tests.personal_hermes import native_e2e as scenarios

pytestmark = [pytest.mark.e2e, pytest.mark.cluster, pytest.mark.security]

BASE = f"https://{scenarios.PUBLIC}"
HTML = {"Accept": "text/html"}
ORIGIN = {"Origin": BASE}
UNAUTHENTICATED = (401, {"error": "invalid_identity_evidence"})
INVALID = (422, {"error": "invalid_request"})
FORBIDDEN = (403, {"error": "access_denied"})


def refusal(status_and_body):
    return {"status": status_and_body[0], "body": status_and_body[1], "cookies": []}


def row(name, method, path, expect, headers=None, **request):
    return pytest.param(method, path, headers or {}, request, expect, id=name)


CASES = [
    # --- GET /_platform/login (beginPlatformLogin)
    row("login", "GET", "/_platform/login", {"status": 303, "login": True}),
    row(
        "login-with-a-local-return-path",
        "GET",
        "/_platform/login?returnTo=%2Fchat%3Fa%3D1",
        {"status": 303, "login": True},
    ),
    *(
        row(
            f"login-refuses-{name}",
            "GET",
            f"/_platform/login?returnTo={quote(value, safe='')}",
            refusal(INVALID),
        )
        for name, value in {
            "a-protocol-relative-url": "//evil.example/x",
            "an-absolute-url": "https://evil.example/",
            "a-platform-path": "/_platform/me",
            "an-auth-path": "/auth/login",
            "an-api-path": "/api/files",
            "a-path-over-2048-bytes": "/" + "a" * 2100,
        }.items()
    ),
    # --- GET /_platform/oidc/callback (completePlatformLogin)
    row(
        "callback-without-parameters",
        "GET",
        "/_platform/oidc/callback",
        refusal(UNAUTHENTICATED),
    ),
    row(
        "callback-with-an-unknown-state-and-code",
        "GET",
        "/_platform/oidc/callback?state=unknown-state&code=unknown-code",
        refusal(UNAUTHENTICATED),
    ),
    row(
        "callback-with-a-provider-error",
        "GET",
        "/_platform/oidc/callback?state=unknown-state&error=access_denied"
        "&error_description=unknown-description",
        refusal(UNAUTHENTICATED),
    ),
    # --- GET /_platform/me (getPlatformSession)
    row("me-without-a-session", "GET", "/_platform/me", refusal(UNAUTHENTICATED)),
    # --- POST /_platform/logout (logoutPlatform)
    row(
        "platform-logout-without-an-origin",
        "POST",
        "/_platform/logout",
        refusal(FORBIDDEN),
    ),
    row(
        "platform-logout-with-an-origin-and-no-session",
        "POST",
        "/_platform/logout",
        {"status": 401, "body": UNAUTHENTICATED[1]},
        ORIGIN,
    ),
    row(
        "platform-logout-with-a-token-and-no-session",
        "POST",
        "/_platform/logout",
        {"status": 401, "body": UNAUTHENTICATED[1]},
        {**ORIGIN, "X-CSRF-Token": "0" * 64},
    ),
    # --- POST /auth/logout (logoutNative)
    row("native-logout-without-an-origin", "POST", "/auth/logout", refusal(FORBIDDEN)),
    row(
        "native-logout-from-another-origin",
        "POST",
        "/auth/logout",
        refusal(FORBIDDEN),
        {"Origin": "https://evil.example"},
    ),
    row(
        "native-logout-from-the-origin-ends-nothing-and-leaves",
        "POST",
        "/auth/logout",
        {"status": 303, "location": "/_platform/signed-out"},
        ORIGIN,
    ),
    # --- GET /_platform/signed-out, GET /_platform/healthz
    row(
        "signed-out",
        "GET",
        "/_platform/signed-out",
        {"status": 200, "page": "signed-out", "cookies": []},
    ),
    row(
        "healthz",
        "GET",
        "/_platform/healthz",
        {"status": 200, "text": "ok", "cookies": []},
    ),
    # --- the proxied application needs a session
    row(
        "a-page-load-is-sent-to-sign-in",
        "GET",
        "/",
        {"status": 303, "location": "/_platform/login?returnTo=%2F", "cookies": []},
        HTML,
    ),
    row(
        "a-page-load-keeps-its-path-and-query",
        "GET",
        "/chat?a=1&b=%C3%A4",
        {
            "status": 303,
            "location": "/_platform/login?returnTo=%2Fchat%3Fa%3D1%26b%3D%25C3%25A4",
            "cookies": [],
        },
        HTML,
    ),
    row(
        "a-head-page-load-is-sent-to-sign-in",
        "HEAD",
        "/",
        {"status": 303, "location": "/_platform/login?returnTo=%2F", "cookies": []},
        HTML,
    ),
    *(
        row(
            f"a-{method.lower()}-request-gets-json-not-a-redirect",
            method,
            "/api/files/upload",
            refusal(UNAUTHENTICATED),
        )
        for method in ("GET", "POST", "PUT", "DELETE")
    ),
    row(
        "a-web-socket-upgrade-is-refused",
        "GET",
        "/api/ws",
        refusal(UNAUTHENTICATED),
        {
            "Connection": "Upgrade",
            "Upgrade": "websocket",
            "Sec-WebSocket-Version": "13",
            "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==",
        },
    ),
    # --- the prefix is the gateway's: an unlisted path is not found, for all
    *(
        row(
            f"an-unlisted-platform-path-{name}-is-not-found",
            method,
            path,
            refusal((404, {"error": "invalid_request"})),
        )
        for name, method, path in (
            ("get", "GET", "/_platform/nope"),
            ("with-a-slash", "GET", "/_platform/"),
            ("bare", "GET", "/_platform"),
            ("nested", "POST", "/_platform/a/b"),
        )
    ),
]


def cookies(reply):
    """Names, and attributes, of the cookies a response sets."""
    return [
        (header.split("=", 1)[0], header)
        for header in reply.raw.headers.getlist("Set-Cookie")
    ]


@pytest.mark.parametrize("method, path, headers, request_args, expect", CASES)
def test_the_gateway_answers_a_client_without_a_session_as_declared(
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
    # Everything the gateway answers itself is private and inert.
    assert reply.headers["x-content-type-options"] == "nosniff"
    assert reply.headers["referrer-policy"] == "no-referrer"
    assert "no-store" in reply.headers["cache-control"]
    if "body" in expect:
        assert reply.json() == expect["body"]
    if "text" in expect:
        assert reply.text == expect["text"]
    if "location" in expect:
        assert reply.headers["location"] == expect["location"]
    if "page" in expect:
        assert "default-src 'none'" in reply.headers["content-security-policy"]
        assert "<a " in reply.text
    if expect.get("login"):
        # To the provider, with a flow bound to this browser by its proof cookie.
        assert reply.headers["location"].startswith(
            scenarios.ISSUER + "/protocol/openid-connect/auth?"
        )
        assert "__Host-hermes-platform-browser" in [n for n, _ in cookies(reply)]
    if "cookies" in expect:
        assert [name for name, _ in cookies(reply)] == expect["cookies"]
    # No refusal reflects what the client sent.
    if reply.status_code >= 400:
        for sent in (
            "unknown-state",
            "unknown-code",
            "unknown-description",
            "evil.example",
        ):
            assert sent not in reply.text


def test_a_login_redirect_binds_a_single_use_flow_to_the_browser(platform):
    first, second = (
        requests.get(
            f"{BASE}/_platform/login",
            verify=platform.ca,
            timeout=20,
            allow_redirects=False,
        )
        for _ in range(2)
    )
    flows = []
    for reply in (first, second):
        assert reply.status_code == 303
        location = urlparse(reply.headers["location"])
        assert (
            f"{location.scheme}://{location.netloc}"
            == scenarios.ISSUER.rsplit("/realms", 1)[0]
        )
        assert location.path == "/realms/agents/protocol/openid-connect/auth"
        query = dict(part.split("=", 1) for part in location.query.split("&"))
        assert query["response_type"] == "code" and query["scope"] == "openid"
        assert query["client_id"] == "hermes-gateway"
        assert query["code_challenge_method"] == "S256"
        assert query["redirect_uri"] == quote(
            f"{BASE}/_platform/oidc/callback", safe=""
        )
        flows.append((query["state"], query["nonce"], query["code_challenge"]))
        proof = [h for n, h in cookies(reply) if n == "__Host-hermes-platform-browser"]
        assert len(proof) == 1, cookies(reply)
        assert re.search(r"; Secure", proof[0]) and "HttpOnly" in proof[0]
        assert "SameSite=Lax" in proof[0] and "Path=/" in proof[0]
        assert "Domain" not in proof[0]
    # Every flow is fresh: state, nonce and PKCE challenge never repeat.
    assert all(a != b for a, b in zip(*flows, strict=True)), flows
