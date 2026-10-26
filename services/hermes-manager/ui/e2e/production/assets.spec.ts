import { test, expect } from "@playwright/test";

// Run by `hermes-http`'s `production_dashboard_browser_csp` test against the
// real server: real cookies, CSRF, grants, contract and CSP.
test("the built dashboard works under the production CSP", async ({
  page,
  context,
}) => {
  const origin = process.env.MANAGER_E2E_ORIGIN!;
  const violations: string[] = [];
  page.on("console", (message) => {
    if (/content security policy|violates.*directive/i.test(message.text()))
      violations.push(message.text());
  });
  await page.exposeFunction("recordCsp", (directive: string) =>
    violations.push(directive),
  );
  await page.addInitScript(() =>
    document.addEventListener("securitypolicyviolation", (event) => {
      void (
        window as unknown as { recordCsp: (directive: string) => Promise<void> }
      ).recordCsp(event.violatedDirective);
    }),
  );
  await context.addCookies(
    (
      [
        ["__Host-hermes-admin", process.env.MANAGER_E2E_SESSION!],
        ["__Host-hermes-admin-browser", process.env.MANAGER_E2E_BROWSER!],
      ] as const
    ).map(([name, value]) => ({
      name,
      value,
      url: origin,
      secure: true,
      httpOnly: true,
      sameSite: "Lax" as const,
    })),
  );

  // A secret, written through the real CSRF-protected API.
  await page.goto("/secrets");
  await page.getByLabel("Name").fill("browser-key");
  await page.getByRole("button", { name: "Write browser-key" }).click();
  const secret = page.getByRole("dialog");
  await secret.getByLabel("Value").fill("sk-browser-fixture");
  await secret.getByRole("button", { name: "Confirm change" }).click();
  await expect(secret.getByText("Change accepted.")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(
    page.getByRole("cell", { name: "browser-key", exact: true }),
  ).toBeVisible();
  expect(await page.content()).not.toContain("sk-browser-fixture");

  // A template: draft in the lazy-loaded editor, then publish.
  await page.goto("/templates");
  await page.getByLabel("ID", { exact: true }).fill("browser-template");
  await page.getByRole("button", { name: "Start draft" }).click();
  await page.getByRole("tab", { name: "JSON" }).click();
  await page.getByRole("textbox", { name: "Configuration JSON editor" }).fill(
    JSON.stringify({
      schemaVersion: 2,
      image: `ghcr.io/example/hermes@sha256:${"a".repeat(64)}`,
      resources: { cpuMillis: 1000, memoryMib: 2048 },
      home: { source: "nvme", sizeGib: 5 },
      secrets: [{ name: "browser-key", env: "OPENAI_API_KEY" }],
    }),
  );
  await page.getByRole("button", { name: "Validate" }).click();
  await expect(
    page.getByText("The manager accepts this definition."),
  ).toBeVisible();
  await page.getByRole("button", { name: "Save as new draft" }).click();
  await page.getByRole("button", { name: "Publish v1" }).click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Confirm change" })
    .click();
  await expect(page.getByText("Change accepted.")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByText("published", { exact: true })).toBeVisible();

  // Every page renders against the real contract.
  for (const [path, text] of [
    ["/users", "Known identities"],
    ["/assignments", "Assignments"],
    ["/terminal-backends", "Terminal backends"],
    ["/storage", "Storage sources"],
    ["/activity", "template.publish"],
    ["/administrators", "Administrator grants"],
  ]) {
    await page.goto(path);
    await expect(page.getByText(text).first()).toBeVisible();
    await expect(page.getByRole("alert")).toHaveCount(0);
  }
  expect(violations).toEqual([]);
});
