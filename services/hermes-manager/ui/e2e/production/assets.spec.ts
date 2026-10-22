import { test, expect } from "@playwright/test";
test("protected production editor, cookie mutations and publication obey CSP", async ({
  page,
  context,
}) => {
  const origin = process.env.MANAGER_E2E_ORIGIN!;
  const csp: string[] = [];
  page.on("console", (message) => {
    if (/content security policy|violates.*directive/i.test(message.text()))
      csp.push(message.text());
  });
  await page.exposeFunction("recordCsp", (directive: string) =>
    csp.push(directive),
  );
  await page.addInitScript(() =>
    document.addEventListener("securitypolicyviolation", (event) => {
      void (
        window as unknown as { recordCsp: (directive: string) => Promise<void> }
      ).recordCsp(event.violatedDirective);
    }),
  );
  await context.addCookies([
    {
      name: "__Host-hermes-admin",
      value: process.env.MANAGER_E2E_SESSION!,
      url: origin,
      secure: true,
      httpOnly: true,
      sameSite: "Lax",
    },
    {
      name: "__Host-hermes-admin-browser",
      value: process.env.MANAGER_E2E_BROWSER!,
      url: origin,
      secure: true,
      httpOnly: true,
      sameSite: "Lax",
    },
  ]);
  await page.goto("/templates/new");
  await page.getByRole("button", { name: "JSON", exact: true }).click();
  await expect(page.getByLabel("Configuration JSON editor")).toBeVisible();
  await page.waitForTimeout(1200);
  await page.goto("/terminal-runtimes");
  await page
    .getByRole("button", { name: "Register runtime", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await page
    .getByLabel("Runtime: Runtime ID", { exact: true })
    .fill("fixture-browser-runtime");
  await page
    .getByLabel("Image pinned by digest")
    .fill(`ghcr.io/example/worker@sha256:${"c".repeat(64)}`);
  await page
    .getByRole("button", { name: "Confirm change", exact: true })
    .click();
  await expect(page.getByText("Change accepted.")).toBeVisible();
  await page
    .getByRole("link", { name: "Inspect recorded configuration change" })
    .click();
  await expect(
    page.getByRole("heading", { name: "Redacted change" }),
  ).toBeVisible();
  await expect(
    page.getByText('"outcome": "Recorded"', { exact: false }),
  ).toBeVisible();
  await page.getByRole("link", { name: "Inspect runtime →" }).click();
  await expect(
    page.getByRole("heading", {
      name: "Complete immutable runtime definition",
    }),
  ).toBeVisible();
  await page.goto("/templates/new");
  await page
    .getByLabel("Template ID", { exact: true })
    .fill("fixture-browser-template");
  await page.getByRole("button", { name: "JSON", exact: true }).click();
  await page.getByRole("textbox", { name: "Configuration JSON editor" }).fill(
    JSON.stringify({
      schemaVersion: 1,
      image: `ghcr.io/example/hermes@sha256:${"a".repeat(64)}`,
      storageClass: "standard",
      homeGib: 8,
      workspaceGib: 16,
      cpuMillis: 1000,
      memoryMib: 1024,
      adminConfig: {
        terminal: { backend: "managed_execution" },
        advanced: { retained: true },
      },
    }),
  );
  await page.getByRole("button", { name: "Save draft", exact: true }).click();
  await expect(page).toHaveURL(/\/template-drafts\/[a-f0-9-]+$/);
  await page
    .getByRole("button", { name: "Review publication", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Publish version", exact: true })
    .click();
  await expect(page.getByText(/Published immutable version 1/)).toBeVisible();
  await page
    .getByRole("navigation")
    .getByRole("link", { name: /Users/ })
    .click();
  await page.getByRole("link", { name: "owner-a", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "User details" }),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "Create assignment", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Create personal instance", exact: true })
    .click();
  await expect(
    page.getByRole("heading", {
      name: "Selected template deployment and managed configuration",
    }),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "Confirm change", exact: true })
    .click();
  await expect(page.getByText("Change accepted.")).toBeVisible();
  await page.keyboard.press("Escape");
  await page
    .getByRole("button", { name: "Close identity picker", exact: true })
    .click();
  await page
    .getByRole("link", { name: "Manage assignment and template →" })
    .click();
  await expect(
    page.getByRole("table", { name: "Assignment operations" }),
  ).toBeVisible();
  await page.getByRole("link", { name: "commission", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "Operation status" }),
  ).toBeVisible();
  await expect(page.getByText("Pending", { exact: true })).toBeVisible();
  expect(csp).toEqual([]);
});
