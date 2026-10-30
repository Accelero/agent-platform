"""A real Chromium for the installed platform's journeys.

The browser trusts the fixture CA through its own certificate database, never a
blanket error bypass, so hostname and chain validation stay on; it resolves the
fixture's hostnames to the node ports with its resolver rules and can reach
nothing else; and it runs from a private home, so no profile of the developer
is read or written. Every case takes its own context: a fresh cookie jar and
storage, as a different person at a different machine would have.

Nothing here prints a credential.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
from contextlib import contextmanager
from pathlib import Path

from playwright.sync_api import sync_playwright

from tests.personal_hermes import native_e2e as e2e

KEYCLOAK_ORIGIN = f"https://{e2e.KEYCLOAK}:8443"
TIMEOUT_MS = 30_000


def trust(home: Path, ca: str):
    """A certificate database in `home` that holds the fixture CA only."""
    if shutil.which("certutil") is None:
        raise RuntimeError("the browser journeys need the NSS tool `certutil`")
    database = home / ".pki" / "nssdb"
    database.mkdir(parents=True)
    for arguments in (
        ["-N", "--empty-password"],
        ["-A", "-n", "fixture-ca", "-t", "C,,", "-i", ca],
    ):
        subprocess.run(
            ["certutil", *arguments, "-d", f"sql:{database}"],
            check=True,
            capture_output=True,
            timeout=60,
        )


def resolver_rules(node_ip: str):
    """Maps the fixture's names to their node ports; every other name fails."""
    mapped = [
        (e2e.KEYCLOAK, e2e.NODE_PORTS["keycloak"]),
        (e2e.PUBLIC, e2e.NODE_PORTS["gateway"]),
        (e2e.ADMIN, e2e.NODE_PORTS["admin"]),
    ]
    rules = [f"MAP {host} {node_ip}:{port}" for host, port in mapped]
    return ", ".join([*rules, "MAP * ~NOTFOUND"])


class Chromium:
    """One browser process; contexts are the people."""

    def __init__(self, node_ip: str, ca: str, home: Path):
        trust(home, ca)
        self.playwright = sync_playwright().start()
        try:
            self.browser = self.playwright.chromium.launch(
                args=[
                    f"--host-resolver-rules={resolver_rules(node_ip)}",
                    # The fixture's node address is private, so Chromium would
                    # refuse every request a "public" page makes to it before
                    # it left the browser. A real attacker's page reaches a
                    # public platform; the cases must exercise the platform's
                    # own defences, not this browser check.
                    "--disable-features=LocalNetworkAccessChecks",
                ],
                env={**os.environ, "HOME": str(home)},
            )
        except BaseException:
            self.playwright.stop()
            raise

    def close(self):
        try:
            self.browser.close()
        finally:
            self.playwright.stop()

    @contextmanager
    def context(self):
        """A new person's browser, with CSP violations, dialogs and page errors
        recorded for the case to inspect."""
        context = self.browser.new_context()
        context.set_default_timeout(TIMEOUT_MS)
        context.set_default_navigation_timeout(TIMEOUT_MS)
        context.observed = Observed(context)
        try:
            yield context
        finally:
            context.close()


class Observed:
    """What the pages of one context did that no assertion on a response
    shows: script dialogs, uncaught errors and Content-Security-Policy
    violations."""

    SCRIPT = """
        document.addEventListener("securitypolicyviolation", (event) => {
            window.recordViolation(event.violatedDirective);
        });
    """

    def __init__(self, context):
        self.dialogs: list[str] = []
        self.errors: list[str] = []
        self.violations: list[str] = []
        context.expose_function(
            "recordViolation", lambda directive: self.violations.append(directive)
        )
        context.add_init_script(self.SCRIPT)
        context.on("page", self.watch)

    def watch(self, page):
        # An alert, confirm or prompt means page script ran where only text
        # was expected; dismiss it so the case fails on its assertion.
        page.on(
            "dialog",
            lambda dialog: (self.dialogs.append(dialog.message), dialog.dismiss()),
        )
        page.on("pageerror", lambda error: self.errors.append(str(error)))


def sign_in(page, url: str, username: str, password: str, landing: str):
    """Opens `url`, signs in at Keycloak's login form and waits for `landing`
    (a URL or a predicate)."""
    page.goto(url)
    page.wait_for_url(re.compile(rf"^{re.escape(KEYCLOAK_ORIGIN)}/"))
    page.fill("#username", username)
    page.fill("#password", password)
    page.click("#kc-login")
    page.wait_for_url(landing)


def fetch(page, path: str, **options):
    """One same-origin `fetch` by the page's own script, with the browser's
    cookies and Origin; returns status, headers and the text body."""
    return page.evaluate(
        """async ([path, options]) => {
            const reply = await fetch(path, options);
            return {
                status: reply.status,
                headers: Object.fromEntries(reply.headers.entries()),
                text: await reply.text(),
            };
        }""",
        [path, options],
    )
