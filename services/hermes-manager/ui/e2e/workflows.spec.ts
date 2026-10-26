import { test, expect } from "@playwright/test";
import { ASSIGNMENT, identity, mockApi } from "./fixtures";

test("create an assignment for a directory user", async ({ page }) => {
  const recorded = await mockApi(page);
  await page.goto("/users?tab=directory");
  await page.getByRole("button", { name: "Create assignment" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("Hermes template: ID").fill("standard");
  await dialog
    .getByLabel("Hermes template: published version")
    .selectOption("1");
  await dialog.getByRole("button", { name: "Add volume" }).click();
  await dialog.getByLabel("Volume name").fill("workspace");
  await dialog.getByLabel("Source").selectOption("host-data");
  await dialog.getByLabel("Path under the source root").fill("carol/work");
  await dialog.getByRole("button", { name: "Confirm assignment" }).click();
  await expect(dialog.getByText("Assignment created.")).toBeVisible();
  const create = recorded.find((r) => r.path === "/api/v1/assignments")!;
  expect(create.headers["x-csrf-token"]).toBe("fixture-csrf");
  expect(create.headers["idempotency-key"]).toMatch(/^[0-9a-f-]{36}$/);
  expect(create.body).toEqual({
    identity: identity("carol"),
    spec: {
      access: "enabled",
      template: { id: "standard", version: 1 },
      volumes: {
        workspace: { source: "host-data", path: "carol/work" },
      },
    },
  });
});

test("change an assignment only after a valid preview", async ({ page }) => {
  const recorded = await mockApi(page);
  await page.goto(`/assignments/${ASSIGNMENT}`);
  await expect(page.getByText("hi-0123456789ab-0b7d4c1e")).toBeVisible();
  await page.getByRole("button", { name: "Change assignment" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("Access").selectOption("suspended");
  const confirm = dialog.getByRole("button", { name: "Confirm change" });
  await expect(confirm).toBeDisabled();
  await dialog.getByRole("button", { name: "Preview effects" }).click();
  await expect(dialog.getByText("Restarts Hermes.")).toBeVisible();
  await confirm.click();
  await expect(dialog.getByText("version 4")).toBeVisible();
  const change = recorded.find((r) => r.method === "PATCH")!;
  expect(change.headers["if-match"]).toBe('"3"');
  expect(change.body).toMatchObject({
    access: "suspended",
    template: { id: "standard", version: 1 },
    terminal: { default: "ssh", allowed: { ssh: 1 } },
  });
});

test("roll a template version out to selected assignments", async ({
  page,
}) => {
  const recorded = await mockApi(page);
  await page.goto("/assignments");
  await page.getByLabel("Select bob").check();
  await page.getByLabel("Template version: ID").fill("standard");
  await page
    .getByLabel("Template version: published version")
    .selectOption("1");
  await page.getByRole("button", { name: "Apply to 1 assignments" }).click();
  await expect(page.getByText("Now version 4")).toBeVisible();
  expect(
    recorded.find((r) => r.path === "/api/v1/assignment-rollouts")!.body,
  ).toEqual({
    template: { id: "standard", version: 1 },
    items: [{ assignmentId: ASSIGNMENT, expectedVersion: 3 }],
  });
});

test("publish a draft with its revision as precondition", async ({ page }) => {
  const recorded = await mockApi(page);
  await page.goto("/templates/standard?version=2");
  await page.getByRole("button", { name: "Publish v2" }).click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Confirm change" })
    .click();
  await expect(page.getByText("Change accepted.")).toBeVisible();
  const publish = recorded.find((r) => r.path.endsWith("/publish"))!;
  expect(publish.path).toBe("/api/v1/templates/standard/versions/2/publish");
  expect(publish.headers["if-match"]).toBe('"4"');
  expect(publish.body).toBeUndefined();
});

test("edit a draft as YAML and see server problems", async ({ page }) => {
  await mockApi(page);
  await page.goto("/templates/standard?version=2");
  await page.getByRole("button", { name: "Edit draft" }).click();
  await expect(page.getByLabel("Configuration YAML editor")).toBeVisible();
  await page.getByRole("tab", { name: "JSON" }).click();
  await expect(page.getByLabel("Configuration JSON editor")).toBeVisible();
  await page.getByRole("button", { name: "Validate" }).click();
  await expect(page.getByText("No admin secret has this name.")).toBeVisible();
});

test("secret values are write-only", async ({ page }) => {
  const recorded = await mockApi(page);
  await page.goto("/secrets");
  await page
    .getByRole("button", { name: "Write new value for llm-key" })
    .click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("Value").fill("sk-fixture-secret");
  await dialog.getByRole("button", { name: "Confirm change" }).click();
  await expect(dialog.getByText("Change accepted.")).toBeVisible();
  const write = recorded.find((r) => r.method === "PUT")!;
  expect(write.body).toEqual({ kind: "env", value: "sk-fixture-secret" });
  await page.keyboard.press("Escape");
  await expect(page.getByText("sk-fixture-secret")).toHaveCount(0);
  expect(await page.content()).not.toContain("sk-fixture-secret");
});
