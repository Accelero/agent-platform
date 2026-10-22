import { test, expect } from "@playwright/test";

test.beforeEach(async ({ page }) => {
  // Presentation fixtures only. Real HTTPS OIDC/session/role authorization is
  // exercised by the Rust PostgreSQL browser tests, never mocked in production.
  await page.route("**/api/v1/**", async (route) => {
    const session = route.request().url().endsWith("/session");
    await route.fulfill({
      json: session
        ? {
            actor: {
              issuer: "https://sso.example.org/realms/agents",
              subject: "fixture-admin",
            },
            permittedActions: ["assignment.create"],
            csrfToken: "fixture-csrf",
          }
        : { items: [], nextCursor: null },
    });
  });
});

test("overview, navigation, empty state and mobile layout", async ({
  page,
}) => {
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: "A place for every agent." }),
  ).toBeVisible();
  await expect(page.getByText("fixture-admin", { exact: true })).toBeVisible();
  await page
    .getByRole("navigation")
    .getByRole("link", { name: /Instances/ })
    .click();
  await expect(
    page.getByText("No assignments are registered yet."),
  ).toBeVisible();
  await page
    .getByRole("navigation")
    .getByRole("link", { name: /Terminal backends/ })
    .click();
  await expect(
    page.getByText("No terminal backends are registered yet."),
  ).toBeVisible();
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(
    page.getByRole("heading", { name: "Terminal backends", exact: true }),
  ).toBeVisible();
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
});

test("expired sessions expose sign-in without retaining private rows", async ({
  page,
}) => {
  await page.route("**/api/v1/session", (route) =>
    route.fulfill({
      status: 401,
      json: { error: "invalid_identity_evidence" },
    }),
  );
  await page.goto("/");
  await expect(page).toHaveURL(/\/auth\/login\?returnTo=/);
  await expect(page.getByText("fixture-admin", { exact: true })).toHaveCount(0);
});

test("logout posts CSRF and replaces the document", async ({ page }) => {
  let loggedOut = false;
  await page.route("**/auth/logout", async (route) => {
    expect(route.request().method()).toBe("POST");
    expect(route.request().headers()["x-csrf-token"]).toBe("fixture-csrf");
    loggedOut = true;
    await route.fulfill({ status: 204 });
  });
  await page.route("**/auth/signed-out", (route) =>
    route.fulfill({
      contentType: "text/html",
      body: '<h1>Signed out</h1><a href="/auth/login">Sign in with Keycloak</a>',
    }),
  );
  await page.goto("/");
  await page.getByRole("button", { name: "Sign out ↗" }).click();
  await expect.poll(() => loggedOut).toBe(true);
  await expect(page.getByRole("heading", { name: "Signed out" })).toBeVisible();
  await expect(page.getByText("fixture-admin", { exact: true })).toHaveCount(0);
});

test("API errors stay visible and do not look like empty data", async ({
  page,
}) => {
  await page.route("**/api/v1/assignments?*", (route) =>
    route.fulfill({ status: 503, json: { error: "dependency_unavailable" } }),
  );
  await page.goto("/assignments");
  await expect(page.getByRole("alert")).toContainText(
    "could not complete this request",
  );
  await expect(
    page.getByText("No assignments are registered yet."),
  ).toHaveCount(0);
});
