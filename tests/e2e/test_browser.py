"""Journeys in a rendered browser on the installed platform.

Chromium trusts the fixture CA and resolves only the fixture's hostnames (see
`tests/support/browser.py`), signs in through Keycloak's own login form and
then acts as a person would: pages, same-origin `fetch`, form posts, logout and
history. The assertions are on what the browser and the platform together
guarantee: cookie scope, who the session belongs to, what a foreign origin can
and cannot do with the owner's cookies, what a dead session still reaches, and
that hostile text is shown as text.

Every case takes its own context, so cookies and storage never carry over.
The platform's own pages carry `default-src 'none'`, which blocks script
requests from them, so cases that read an API as the page's script go to the
host's liveness endpoint first (`at`), which has no policy.
"""

import base64
import json
import re
import secrets
from urllib.parse import quote, urlparse

import pytest

from tests.personal_hermes import native_e2e as e2e
from tests.support.browser import fetch, sign_in

pytestmark = [
    pytest.mark.e2e,
    pytest.mark.cluster,
    pytest.mark.browser,
    pytest.mark.slow,
]

PUBLIC = f"https://{e2e.PUBLIC}"
ADMIN = f"https://{e2e.ADMIN}"
HEALTH = {PUBLIC: "/_platform/healthz", ADMIN: "/healthz"}
PLATFORM_COOKIES = ("__Host-hermes-platform", "__Host-hermes-platform-browser")
# Both are foreign to the platform's origins; only the second shares their
# registrable domain, so only there do the owner's Lax cookies travel.
SITES = ["attacker.example", "attacker.e2e.test"]
REFUSED = (400, 401, 403, 415, 422)


def left_the_login(host):
    """True once the browser is on `host`, past any login or callback path."""

    def landed(url):
        parsed = urlparse(url)
        return parsed.hostname == host and not parsed.path.startswith(
            ("/_platform/", "/auth/oidc/")
        )

    return landed


def on(host):
    """True once the browser is on `host`, wherever; a refusal is a landing."""
    return lambda url: urlparse(url).hostname == host


def at(page, origin):
    """Moves `page` to the liveness endpoint of `origin`, whose response has no
    Content-Security-Policy, so the page's script can read that origin's API."""
    page.goto(f"{origin}{HEALTH[origin]}")
    return page


def owner(context, platform, name):
    """A page signed in as `name` on the gateway, through Keycloak's form."""
    page = context.new_page()
    sign_in(
        page,
        f"{PUBLIC}/",
        name,
        platform.passwords[name],
        left_the_login(e2e.PUBLIC),
    )
    return page


def administrator(context, platform):
    """A page signed in as Alice on the dashboard, through Keycloak's form."""
    page = context.new_page()
    sign_in(
        page,
        f"{ADMIN}/",
        "alice",
        platform.passwords["alice"],
        left_the_login(e2e.ADMIN),
    )
    return page


def native_session(page):
    """Native Hermes's own login, which reuses the Keycloak session; returns
    the native account."""
    providers = json.loads(fetch(page, "/api/auth/providers")["text"])
    listed = providers.get("providers", providers)
    name = next(item["name"] if isinstance(item, dict) else item for item in listed)
    page.goto(f"{PUBLIC}/auth/login?provider={quote(name)}&next=/")
    reply = fetch(page, "/api/auth/me")
    assert reply["status"] == 200, f"native session: {reply['status']}"
    return json.loads(reply["text"])


def platform_cookies(context, origin=PUBLIC):
    return {
        cookie["name"]: cookie
        for cookie in context.cookies(origin)
        if cookie["name"] in PLATFORM_COOKIES
    }


def upload(page, path, content):
    """Native Hermes's managed file upload, as the page's own script."""
    encoded = base64.b64encode(content.encode()).decode()
    return fetch(
        page,
        "/api/files/upload",
        method="POST",
        headers={"Content-Type": "application/json"},
        body=json.dumps(
            {
                "path": path,
                "data_url": f"data:text/plain;base64,{encoded}",
                "overwrite": True,
            }
        ),
    )


def known_person(platform, chromium, prefix):
    """A new directory person who has signed in once at the dashboard, which
    is refused (no grant) but makes the identity known to the manager."""
    username = f"{prefix}-{secrets.token_hex(3)}"
    password = secrets.token_urlsafe(18)
    directory = e2e.Directory(platform.ca, platform.passwords["directory"])
    subject = directory.create(username, f"{username}@e2e.test", password)
    with chromium.context() as context:
        sign_in(context.new_page(), f"{ADMIN}/", username, password, on(e2e.ADMIN))
        assert "__Host-hermes-admin" not in {c["name"] for c in context.cookies(ADMIN)}
    return username, password, subject


def test_an_owner_signs_in_through_keycloak_and_the_session_cookies_are_confined(
    platform, chromium
):
    with chromium.context() as context:
        page = context.new_page()
        sign_in(
            page,
            f"{PUBLIC}/",
            "bob",
            platform.passwords["bob"],
            left_the_login(e2e.PUBLIC),
        )
        # The provider's code and state are consumed, never left in the URL.
        assert not re.search(r"[?&](code|state|session_state)=", page.url), page.url
        cookies = platform_cookies(context)
        assert set(cookies) == set(PLATFORM_COOKIES), "the session cookies are absent"
        for cookie in cookies.values():
            assert cookie["secure"] and cookie["httpOnly"], cookie["name"]
            assert cookie["sameSite"] == "Lax", cookie["name"]
            assert cookie["path"] == "/", cookie["name"]
            assert cookie["domain"] == e2e.PUBLIC, "the cookie is not host-only"
        # Page script cannot read them, and they go to no other host.
        assert "__Host-hermes" not in page.evaluate("document.cookie")
        scattered = [
            (cookie["name"], cookie["domain"])
            for cookie in context.cookies()
            if cookie["name"].startswith("__Host-hermes-platform")
            and cookie["domain"] != e2e.PUBLIC
        ]
        assert not scattered, scattered
        me = fetch(page, "/_platform/me")
        assert me["status"] == 200
        assert "no-store" in me["headers"]["cache-control"]
        assert json.loads(me["text"])["identity"] == {
            "issuer": e2e.ISSUER,
            "subject": e2e.SUBJECTS["bob"],
        }
        assert not context.observed.errors, context.observed.errors


def test_an_unknown_account_is_told_it_has_no_hermes_and_reaches_nothing(
    platform, chromium
):
    with chromium.context() as context:
        page = context.new_page()
        sign_in(
            page,
            f"{PUBLIC}/",
            "mallory",
            platform.passwords["mallory"],
            left_the_login(e2e.PUBLIC),
        )
        assert "No Hermes for this account" in page.content()
        denial = page.goto(f"{PUBLIC}/")
        assert denial.status == 403
        # The denial page is inert: no script, no request, no framing.
        policy = denial.headers["content-security-policy"]
        assert "default-src 'none'" in policy and "frame-ancestors 'none'" in policy
        assert "no-store" in denial.headers["cache-control"]
        assert page.locator("script").count() == 0
        # Without an assignment nothing of any instance is reachable.
        at(page, PUBLIC)
        refused = fetch(page, "/api/auth/me")
        assert refused["status"] == 403, refused
        assert json.loads(refused["text"]) == {"error": "access_denied"}
        assert not context.observed.dialogs


@pytest.mark.security
def test_each_owners_browser_reaches_only_their_own_instance(platform, chromium):
    with chromium.context() as bob_context, chromium.context() as carol_context:
        pages = {
            "bob": owner(bob_context, platform, "bob"),
            "carol": owner(carol_context, platform, "carol"),
        }
        for name, page in pages.items():
            account = native_session(page)
            assert account["user_id"] == e2e.SUBJECTS[name], f"{name} reached another"
        uploaded = upload(pages["bob"], "uploads/browser.txt", secrets.token_hex(8))
        assert uploaded["status"] == 200, uploaded["status"]
        ours = fetch(pages["bob"], "/api/files?path=uploads")
        assert ours["status"] == 200 and "browser.txt" in ours["text"]
        theirs = fetch(pages["carol"], "/api/files?path=uploads")
        assert "browser.txt" not in theirs["text"], "Carol's browser lists Bob's file"


def attempts(host, targets):
    """A page that, from its own origin, sends a credentialed POST to each of
    `targets` on `host`, once as a simple request and once as a JSON one that
    needs a preflight."""
    body = json.dumps(
        {
            "path": "uploads/forged.txt",
            "data_url": "data:text/plain;base64,Zm9yZ2Vk",
            "overwrite": True,
            "identity": {"issuer": e2e.ISSUER, "subject": "forged"},
            "spec": {"access": "enabled"},
        }
    )
    return (
        """<!doctype html><title>elsewhere</title><script>
window.attempts = (async () => {
  const body = BODY;
  const outcomes = [];
  for (const target of TARGETS) {
    for (const mode of ["no-cors", "cors"]) {
      try {
        const reply = await fetch("https://HOST" + target, {
          method: "POST", mode, credentials: "include",
          headers: {
            "Content-Type": mode === "cors" ? "application/json" : "text/plain",
          },
          body,
        });
        outcomes.push([target, mode, reply.type]);
      } catch (error) {
        outcomes.push([target, mode, "blocked"]);
      }
    }
  }
  return outcomes;
})();
</script>""".replace("BODY", json.dumps(body))
        .replace("TARGETS", json.dumps(targets))
        .replace("HOST", host)
    )


def form_post(host, path):
    """A page that submits a form to `path` on `host` as it loads."""
    return f"""<!doctype html><title>elsewhere</title>
<form id="f" method="POST" action="https://{host}{path}"></form>
<script>document.getElementById("f").submit()</script>"""


def forge(context, site, source, expect):
    """Loads `source` as the page at `https://{site}/` and returns the answers
    (method, path, status) of every POST it made to the platform's hosts.
    `expect` is "script" to wait for the page's requests to finish, or the
    pattern of the URL its form post navigates to."""

    def serve(route):
        route.fulfill(status=200, content_type="text/html", body=source)

    context.route(f"https://{site}/**", serve)
    page = context.new_page()
    answers = []
    page.on(
        "response",
        lambda reply: (
            answers.append(
                (reply.request.method, urlparse(reply.url).path, reply.status)
            )
            if reply.request.method == "POST"
            and urlparse(reply.url).hostname in (e2e.PUBLIC, e2e.ADMIN)
            else None
        ),
    )
    try:
        if expect == "script":
            page.goto(f"https://{site}/", wait_until="commit")
            page.evaluate("window.attempts")
        else:
            with page.expect_navigation(url=re.compile(expect)):
                page.goto(f"https://{site}/", wait_until="commit")
    finally:
        page.close()
        context.unroute(f"https://{site}/**")
    return answers


@pytest.mark.security
@pytest.mark.parametrize("site", SITES)
def test_another_origin_cannot_act_with_the_owners_cookies(platform, chromium, site):
    """A page of another origin, a sibling host of the same site included, can
    make the owner's browser send a request with the owner's cookies. The
    gateway refuses what changes state from any origin but its own, and a
    refused logout expires nothing, so the owner stays signed in with their
    files as they were."""
    same_site = site.endswith("e2e.test")
    with chromium.context() as context:
        page = owner(context, platform, "bob")
        native_session(page)
        answers = forge(
            context,
            site,
            attempts(e2e.PUBLIC, ["/api/files/upload", "/_platform/logout"]),
            "script",
        )
        statuses = {path: status for _, path, status in answers}
        # From another site the cookies do not travel at all; from a sibling
        # host they do, and the Origin is what refuses the request.
        assert statuses == {
            "/api/files/upload": 401 if not same_site else 403,
            "/_platform/logout": 403,
        }, answers
        # A top-level form post is the other classic forgery.
        navigated = forge(
            context, site, form_post(e2e.PUBLIC, "/auth/logout"), r"/auth/logout$"
        )
        assert navigated == [("POST", "/auth/logout", 403)], navigated
        # Nothing happened to Bob: still signed in, with every cookie, and
        # nothing written.
        assert set(platform_cookies(context)) == set(PLATFORM_COOKIES)
        at(page, PUBLIC)
        assert fetch(page, "/_platform/me")["status"] == 200
        listing = fetch(page, "/api/files?path=uploads")
        assert "forged.txt" not in listing["text"], "a foreign origin wrote a file"


def replayed(chromium, cookies, paths):
    """The status each of `paths` answers to a browser that was handed
    `cookies`, as Playwright reports them: another person's whole cookie jar
    for the gateway, platform and native cookies alike."""
    with chromium.context() as stranger:
        stranger.add_cookies(
            [
                {
                    "name": cookie["name"],
                    "value": cookie["value"],
                    "url": PUBLIC,
                    "secure": cookie["secure"],
                    "httpOnly": cookie["httpOnly"],
                    "sameSite": cookie["sameSite"],
                }
                for cookie in cookies
            ]
        )
        page = at(stranger.new_page(), PUBLIC)
        return {path: fetch(page, path)["status"] for path in paths}


@pytest.mark.security
def test_logout_ends_the_session_and_its_cookies_reach_nothing(platform, chromium):
    with chromium.context() as context:
        page = owner(context, platform, "bob")
        native_session(page)
        # The case makes its own data, so it reads the same in any order.
        assert upload(page, "uploads/logout.txt", "kept")["status"] == 200
        jar = context.cookies(PUBLIC)
        old = {c["name"]: c["value"] for c in jar if c["name"] in PLATFORM_COOKIES}
        assert set(old) == set(PLATFORM_COOKIES)
        # Control: before logout, the jar reaches the owner's data.
        paths = ("/_platform/me", "/api/auth/me", "/api/files?path=uploads")
        alive = replayed(chromium, jar, paths)
        assert alive == dict.fromkeys(paths, 200), alive
        # As the native UI signs out: a form post to its own /auth/logout.
        with page.expect_navigation(url=f"{PUBLIC}/_platform/signed-out"):
            page.evaluate(
                """() => {
                    const form = document.createElement("form");
                    form.method = "POST";
                    form.action = "/auth/logout";
                    document.body.append(form);
                    form.submit();
                }"""
            )
        assert not platform_cookies(context), "the browser kept a session cookie"
        assert page.locator("a").count() >= 1, "no way to sign in again"
        # The ended session is dead for anyone who still holds its cookies.
        dead = replayed(chromium, jar, paths)
        assert dead == dict.fromkeys(paths, 401), dead
        # Going back to the instance's page does not revive the ended session.
        # The provider's own session outlives the platform's, so the browser
        # may be signed in again silently; that is a new session.
        page.go_back()
        page.wait_for_load_state()
        again = platform_cookies(context)
        if again:
            for name, cookie in again.items():
                assert cookie["value"] != old[name], f"{name} was revived"
        else:
            at(page, PUBLIC)
            for path in ("/api/auth/me", "/api/files?path=uploads"):
                assert fetch(page, path)["status"] != 200, path
        assert not context.observed.dialogs


@pytest.mark.security
def test_hostile_text_in_the_callback_is_never_reflected_or_executed(
    platform, chromium
):
    payload = "<img src=x onerror=alert(1)><script>alert(2)</script>"
    with chromium.context() as context:
        page = context.new_page()
        for origin, path in (
            (PUBLIC, "/_platform/oidc/callback"),
            (ADMIN, "/auth/oidc/callback"),
        ):
            reply = page.goto(
                f"{origin}{path}?state={quote(payload)}&error={quote(payload)}"
                f"&error_description={quote(payload)}"
            )
            assert reply.status in REFUSED, (origin, reply.status)
            assert payload not in page.content(), f"{origin} reflected the payload"
            assert "<script>alert" not in page.content()
        for origin in (PUBLIC, ADMIN):
            page.goto(f"{origin}/_platform/login?returnTo={quote(payload)}")
            assert payload not in page.content()
        assert not context.observed.dialogs, context.observed.dialogs
        assert not context.observed.errors, context.observed.errors


@pytest.mark.security
def test_hostile_catalog_text_is_shown_as_text_in_the_dashboard(platform, chromium):
    marker = secrets.token_hex(4)
    payload = (
        f'<img src=x onerror="window.pwned=1"><script>window.pwned=2</script>{marker}'
    )
    template = f"hostile-{marker}"
    platform.admin.publish(
        "templates",
        template,
        {
            "schemaVersion": 2,
            "image": platform.images["hermes"],
            "resources": {"cpuMillis": 1000, "memoryMib": 2048},
            "home": {"source": "local", "sizeGib": 1},
            "adminConfig": {"display": {"note": payload}},
        },
    )
    with chromium.context() as context:
        page = administrator(context, platform)
        page.goto(f"{ADMIN}/templates")
        page.get_by_text(template, exact=True).first.click()
        page.get_by_text(marker).first.wait_for()
        assert page.evaluate("window.pwned") is None, "the stored text ran as script"
        assert not context.observed.dialogs
        assert not context.observed.errors, context.observed.errors
        assert not context.observed.violations, context.observed.violations
        # The markup is on the page as characters, not as elements.
        assert page.locator("img[src='x']").count() == 0


@pytest.mark.security
def test_the_dashboard_signs_an_administrator_in_under_a_strict_policy(
    platform, chromium
):
    with chromium.context() as context:
        page = administrator(context, platform)
        cookies = {
            cookie["name"]: cookie
            for cookie in context.cookies(ADMIN)
            if cookie["name"].startswith("__Host-hermes-admin")
        }
        assert set(cookies) == {"__Host-hermes-admin", "__Host-hermes-admin-browser"}
        for cookie in cookies.values():
            assert cookie["secure"] and cookie["httpOnly"], cookie["name"]
            assert cookie["sameSite"] == "Lax", cookie["name"]
            assert cookie["domain"] == e2e.ADMIN and cookie["path"] == "/"
        assert "__Host-hermes" not in page.evaluate("document.cookie")
        document = page.goto(f"{ADMIN}/")
        headers = document.headers
        assert "no-store" in headers["cache-control"]
        assert headers["x-content-type-options"] == "nosniff"
        assert headers["referrer-policy"] == "no-referrer"
        script = next(
            directive
            for directive in headers["content-security-policy"].split(";")
            if directive.strip().startswith("script-src")
        )
        assert script.split() == ["script-src", "'self'"], script
        session = fetch(page, "/api/v1/session")
        assert session["status"] == 200
        assert json.loads(session["text"])["actor"]["subject"] == e2e.SUBJECTS["alice"]
        # The dashboard renders, and its own scripts break no policy.
        page.get_by_role("navigation").first.wait_for()
        assert not context.observed.violations, context.observed.violations
        assert not context.observed.errors, context.observed.errors


@pytest.mark.security
def test_an_owner_without_a_grant_is_refused_the_dashboard(platform, chromium):
    with chromium.context() as context:
        page = context.new_page()
        sign_in(page, f"{ADMIN}/", "bob", platform.passwords["bob"], on(e2e.ADMIN))
        # The flow's browser-proof cookie may remain; the session must not exist.
        names = {cookie["name"] for cookie in context.cookies(ADMIN)}
        assert "__Host-hermes-admin" not in names, "a session without a grant"
        at(page, ADMIN)
        assert fetch(page, "/api/v1/session")["status"] == 401
        assert fetch(page, "/api/v1/assignments?limit=1")["status"] == 401


@pytest.mark.security
@pytest.mark.parametrize("site", SITES)
def test_another_origin_cannot_use_an_administrators_dashboard_cookies(
    platform, chromium, site
):
    """The dashboard's mutations need the CSRF token and exact Origin, so a
    foreign page's simple cross-origin POST changes nothing, whether or not the
    administrator's cookies travel with it, and a refused logout expires none."""
    with chromium.context() as context:
        page = administrator(context, platform)
        answers = forge(
            context,
            site,
            attempts(e2e.ADMIN, ["/api/v1/assignments", "/auth/logout"]),
            "script",
        )
        assert {path for _, path, _ in answers} == {
            "/api/v1/assignments",
            "/auth/logout",
        }, answers
        for method, path, status in answers:
            assert status in REFUSED, (method, path, status)
        # The forgeries named a made-up identity; none became an assignment, and
        # the administrator is still signed in with every cookie.
        listed = platform.admin.call("GET", "/api/v1/assignments?limit=50").json()
        assert "forged" not in json.dumps(listed), "a foreign origin made an assignment"
        names = {c["name"] for c in context.cookies(ADMIN)}
        assert {"__Host-hermes-admin", "__Host-hermes-admin-browser"} <= names
        at(page, ADMIN)
        assert fetch(page, "/api/v1/session")["status"] == 200


def test_a_dashboard_action_commissions_an_instance_its_owner_uses_in_a_browser(
    platform, chromium
):
    username, password, subject = known_person(platform, chromium, "dora")
    with (
        chromium.context() as admin_context,
        chromium.context() as dora_context,
        chromium.context() as bob_context,
    ):
        dashboard = administrator(admin_context, platform)
        dashboard.goto(f"{ADMIN}/users")
        dashboard.get_by_role("link", name=re.compile(subject)).first.click()
        dashboard.get_by_role("button", name="Create assignment").click()
        dialog = dashboard.get_by_role("dialog")
        dialog.get_by_label("Hermes template: ID").fill("personal")
        version = str(platform.state.template)
        dialog.locator(f"option[value='{version}']").wait_for(state="attached")
        dialog.get_by_label("Hermes template: published version").select_option(version)
        dialog.get_by_role("button", name="Confirm assignment").click()
        # The identity now has an assignment: the dialog gives way to a link.
        dashboard.get_by_role("link", name="Open assignment").wait_for()
        assert not admin_context.observed.violations, admin_context.observed.violations

        listed = platform.admin.call("GET", "/api/v1/assignments?limit=50").json()
        assignment = next(
            item for item in listed["items"] if item["identity"]["subject"] == subject
        )
        e2e.wait(
            lambda: platform.admin.phase(assignment["instanceId"])["phase"] == "ready",
            e2e.READY_TIMEOUT,
            lambda: platform.admin.phase(assignment["instanceId"]),
            interval=5,
        )
        # The dashboard shows the instance as ready.
        dashboard.goto(f"{ADMIN}/assignments")
        row = dashboard.get_by_role("row").filter(has_text=subject)
        row.get_by_text("ready", exact=True).wait_for()
        # The new owner signs in with a browser of their own and uses it.
        dora = dora_context.new_page()
        sign_in(dora, f"{PUBLIC}/", username, password, left_the_login(e2e.PUBLIC))
        assert native_session(dora)["user_id"] == subject
        assert upload(dora, "uploads/dashboard.txt", "made here")["status"] == 200
        assert "dashboard.txt" in fetch(dora, "/api/files?path=uploads")["text"]
        # Another owner's browser neither sees it nor is it theirs.
        bob = owner(bob_context, platform, "bob")
        assert native_session(bob)["user_id"] == e2e.SUBJECTS["bob"]
        assert "dashboard.txt" not in fetch(bob, "/api/files?path=uploads")["text"]
        assert not dora_context.observed.dialogs


def assign(platform, username):
    """A new directory person with an assignment on the published `personal`
    template; returns the assignment's receipt."""
    directory = e2e.Directory(platform.ca, platform.passwords["directory"])
    subject = directory.create(username, f"{username}@e2e.test", secrets.token_hex(12))
    return platform.admin.call(
        "POST",
        "/api/v1/assignments",
        {
            "identity": {"issuer": e2e.ISSUER, "subject": subject},
            "spec": {
                "access": "enabled",
                "template": {"id": "personal", "version": platform.state.template},
            },
        },
    ).json()


def suspend_through_the_dialog(page, assignment):
    """Opens the change dialog on `assignment`, sets access to suspended and
    previews it; returns the dialog."""
    page.goto(f"{ADMIN}/assignments/{assignment}")
    page.get_by_role("button", name="Change assignment").click()
    dialog = page.get_by_role("dialog")
    dialog.get_by_label("Access").select_option("suspended")
    dialog.get_by_role("button", name="Preview effects").click()
    dialog.get_by_text("Valid change").wait_for()
    return dialog


def test_a_stale_dashboard_change_is_refused_and_overwrites_nothing(platform, chromium):
    assignment = assign(platform, f"stale-{secrets.token_hex(3)}")["assignmentId"]
    with chromium.context() as first, chromium.context() as second:
        ours = administrator(first, platform)
        theirs = administrator(second, platform)
        first_dialog = suspend_through_the_dialog(ours, assignment)
        second_dialog = suspend_through_the_dialog(theirs, assignment)
        first_dialog.get_by_role("button", name="Confirm change").click()
        first_dialog.get_by_text("Change accepted as version").wait_for()
        accepted = platform.admin.call("GET", f"/api/v1/assignments/{assignment}")
        version = accepted.json()["version"]
        # The other administrator's dialog still holds the old revision.
        second_dialog.get_by_role("button", name="Confirm change").click()
        second_dialog.get_by_text(
            "The record changed. Refresh before trying again."
        ).wait_for()
        second_dialog.get_by_text("Close this dialog and reopen it").wait_for()
        assert second_dialog.get_by_role("button", name="Confirm change").is_disabled()
        current = platform.admin.call("GET", f"/api/v1/assignments/{assignment}").json()
        assert current["version"] == version, "the stale change was applied"
        assert current["spec"]["access"] == "suspended"
        assert not first.observed.violations and not second.observed.violations


@pytest.mark.security
def test_a_revoked_administrator_is_refused_on_the_next_dashboard_request(
    platform, chromium
):
    username, password, subject = known_person(platform, chromium, "ops")
    platform.admin.call(
        "POST",
        "/api/v1/administrators",
        {
            "identity": {"issuer": e2e.ISSUER, "subject": subject},
            "canManageAdministrators": False,
        },
    )
    with chromium.context() as context:
        page = context.new_page()
        sign_in(page, f"{ADMIN}/", username, password, left_the_login(e2e.ADMIN))
        page.goto(f"{ADMIN}/assignments")
        page.get_by_role("table").first.wait_for()
        assert fetch(page, "/api/v1/assignments?limit=1")["status"] == 200
        # Alice revokes the grant while the page is open.
        granted = next(
            item
            for item in platform.admin.call(
                "GET", "/api/v1/administrators?limit=50"
            ).json()["items"]
            if item["identity"]["subject"] == subject
        )
        current = platform.admin.call(
            "GET", f"/api/v1/administrators/{granted['identityId']}"
        )
        platform.admin.call(
            "PATCH",
            f"/api/v1/administrators/{granted['identityId']}",
            {"active": False, "canManageAdministrators": False},
            etag=current.headers["ETag"],
        )
        # Revocation fences the browser session on its next request.
        assert fetch(page, "/api/v1/assignments?limit=1")["status"] in (401, 403)
        assert fetch(page, "/api/v1/session")["status"] in (401, 403)
        page.goto(f"{ADMIN}/assignments")
        assert page.get_by_role("table").count() == 0, "data is still shown"
