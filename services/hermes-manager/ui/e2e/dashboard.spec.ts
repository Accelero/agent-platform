import { test, expect } from "@playwright/test";
import { mockApi } from "./fixtures";

test("overview, navigation, lists and mobile layout", async ({ page }) => {
  await mockApi(page);
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: "A place for every agent." }),
  ).toBeVisible();
  await expect(page.getByText("fixture-admin", { exact: true })).toBeVisible();
  const nav = page.getByRole("navigation");
  await nav.getByRole("link", { name: /Assignments/ }).click();
  await expect(page.getByRole("link", { name: /^bob/ })).toBeVisible();
  await nav.getByRole("link", { name: /Terminal backends/ }).click();
  await expect(page.getByRole("link", { name: "ssh" })).toBeVisible();
  await nav.getByRole("link", { name: /Storage/ }).click();
  await expect(page.getByText("server-1:/srv/hermes")).toBeVisible();
  await expect(page.getByText("bob/workspace")).toBeVisible();
  await expect(page.getByText("hermes-stray", { exact: true })).toBeVisible();
  await expect(
    page.getByText("Last scanned 2026-10-22T08:05:00Z."),
  ).toBeVisible();
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(
    page.getByRole("heading", { name: "Storage", exact: true }),
  ).toBeVisible();
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
});

test("expired sessions redirect to sign-in without private rows", async ({
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
  await mockApi(page);
  let loggedOut = false;
  await page.route("**/auth/logout", async (route) => {
    expect(route.request().method()).toBe("POST");
    expect(route.request().headers()["x-csrf-token"]).toBe("fixture-csrf");
    loggedOut = true;
    await route.fulfill({ status: 204 });
  });
  await page.route("**/auth/signed-out", (route) =>
    route.fulfill({ contentType: "text/html", body: "<h1>Signed out</h1>" }),
  );
  await page.goto("/");
  await page.getByRole("button", { name: "Sign out ↗" }).click();
  await expect.poll(() => loggedOut).toBe(true);
  await expect(page.getByRole("heading", { name: "Signed out" })).toBeVisible();
});

test("API errors stay visible and never look like empty data", async ({
  page,
}) => {
  await mockApi(page);
  await page.route("**/api/v1/assignments?*", (route) =>
    route.fulfill({ status: 503, json: { error: "dependency_unavailable" } }),
  );
  await page.goto("/assignments");
  await expect(page.getByRole("alert")).toContainText("unavailable");
  await expect(page.getByText("No assignments match.")).toHaveCount(0);
});

test("administrators without the manage grant cannot change grants", async ({
  page,
}) => {
  await mockApi(page, { permitted: ["administer"] });
  await page.goto("/administrators");
  await expect(page.getByRole("link", { name: /fixture-admin/ })).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "Add an administrator" }),
  ).toHaveCount(0);
  await page.goto("/secrets");
  await expect(
    page.getByRole("button", { name: "Write new value for llm-key" }),
  ).toBeVisible();
});

test("an unconfigured directory says so", async ({ page }) => {
  await mockApi(page, { directory: false });
  await page.goto("/users?tab=directory");
  await expect(page.getByText("not configured")).toBeVisible();
});

test("a failed terminal worker shows its reason on the assignment", async ({
  page,
}) => {
  await mockApi(page);
  await page.goto("/assignments");
  await page.getByRole("link", { name: /^bob/ }).click();
  const environments = page.locator("section", {
    has: page.getByRole("heading", { name: "Terminal environments" }),
  });
  await expect(environments).toContainText("ssh · v1");
  await expect(environments).toContainText("draining");
  await expect(environments).toContainText("Image cannot be pulled");
});
