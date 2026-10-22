import { test, expect } from "@playwright/test";
const issuer = "https://sso.example.org/realms/agents";
const id = "00000000-0000-4000-8000-000000000001";
const instanceId = "00000000-0000-4000-8000-000000000002";
const operationId = "00000000-0000-4000-8000-000000000003";
const assignment = {
  id,
  instanceId,
  identity: { issuer, subject: "fixture-owner" },
  source: "manager",
  access: "enabled",
  template: { id: "fixture-template", version: 1 },
  version: 1,
  namespace: "fixture-instance",
  desiredGeneration: 1,
  observedGeneration: 1,
  state: "Ready",
};
const permissions = [
  "assignment.create",
  "assignment.set_access",
  "instance.retry",
  "execution.runtime.register",
  "execution.storage.register",
  "execution.storage.update",
  "execution.pull-secret.register",
  "execution.policy.set",
  "template.draft.create",
  "template.draft.update",
  "template.publish",
  "assignment.set_template",
  "administrator.manage",
];
const definition = {
  schemaVersion: 1,
  image: `ghcr.io/example/hermes@sha256:${"a".repeat(64)}`,
  storageClass: "standard",
  homeGib: 8,
  workspaceGib: 16,
  cpuMillis: 1000,
  memoryMib: 1024,
  adminConfig: { advanced: { keep: [1, true, null] } },
};
test.beforeEach(async ({ page }) => {
  await page.route("**/api/v1/**", async (route) => {
    const url = new URL(route.request().url());
    let json: unknown = { items: [], nextCursor: null };
    if (url.pathname.endsWith("/session"))
      json = {
        actor: { issuer, subject: "fixture-admin" },
        permittedActions: permissions,
        csrfToken: "fixture-csrf",
      };
    else if (url.pathname === "/api/v1/assignments")
      json = {
        items: [
          {
            ...assignment,
            identity: {
              issuer,
              subject: url.searchParams.has("after")
                ? "fixture-second"
                : "fixture-owner",
            },
          },
        ],
        nextCursor: url.searchParams.has("after") ? null : "fixture-cursor",
      };
    else if (url.pathname === `/api/v1/assignments/${id}`) json = assignment;
    else if (url.pathname === `/api/v1/instances/${instanceId}`)
      json = { ...assignment, id: instanceId, assignmentId: id };
    else if (url.pathname.endsWith("/execution"))
      json = {
        assignmentId: id,
        assignmentVersion: 1,
        desiredGeneration: 1,
        appliedGeneration: 1,
        state: "Applied",
        desired: { defaultRuntimeId: "python-tools", runtimes: [] },
        digest: "a".repeat(64),
      };
    else if (url.pathname === "/api/v1/terminal-runtimes/python-tools")
      json = {
        schemaVersion: 1,
        runtime: { id: "python-tools", version: 1 },
        image: definition.image,
        sshContract: "managed-ssh-v2",
        uid: 10001,
        gid: 10001,
        serviceAccount: "worker",
        resources: { cpuMillis: 100, memoryMib: 256, ephemeralStorageMib: 256 },
        locale: "C.UTF-8",
        egress: [],
        pullSecretNames: [],
      };
    else if (
      url.pathname.startsWith(
        "/api/v1/template-definitions/fixture-template/versions/",
      )
    )
      json = {
        id: "fixture-template",
        version: 1,
        digest: "a".repeat(64),
        definition,
        redactedPaths: [],
      };
    else if (
      url.pathname === "/api/v1/templates" ||
      url.pathname === "/api/v1/template-versions"
    )
      json = {
        items: [
          {
            id: "fixture-template",
            version: 1,
            image: definition.image,
            nativePort: 8642,
            nativeScheme: "http",
          },
        ],
      };
    else if (url.pathname === "/api/v1/identities")
      json = {
        items: [
          { id, identity: assignment.identity, directoryVerified: false },
        ],
        nextCursor: null,
      };
    else if (url.pathname.startsWith("/api/v1/operations/"))
      json = {
        id: operationId,
        instanceId,
        kind: "commission",
        generation: 1,
        state: "Succeeded",
        attempt: 0,
        nextAttemptAt: 0,
      };
    await route.fulfill({ json, headers: { ETag: '"1"' } });
  });
  await page.route("**/auth/login?*", (route) =>
    route.fulfill({ contentType: "text/html", body: "<h1>Sign in</h1>" }),
  );
});
test("client navigation retains the document and cursor/search URLs survive reload", async ({
  page,
}) => {
  await page.goto("/");
  await page.evaluate(() => {
    (window as unknown as { fixtureDocument: boolean }).fixtureDocument = true;
  });
  await page
    .getByRole("navigation")
    .getByRole("link", { name: /Instances/ })
    .click();
  expect(
    await page.evaluate(
      () => (window as unknown as { fixtureDocument: boolean }).fixtureDocument,
    ),
  ).toBe(true);
  await expect(
    page.getByRole("link", { name: "fixture-owner", exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Next", exact: true }).click();
  await expect(
    page.getByRole("link", { name: "fixture-second" }),
  ).toBeVisible();
  await expect(page).toHaveURL(/cursors=/);
  await page.reload();
  await expect(
    page.getByRole("link", { name: "fixture-second" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Previous", exact: true }).click();
  await expect(
    page.getByRole("link", { name: "fixture-owner", exact: true }),
  ).toBeVisible();
  await page.getByLabel("Search assignments").fill("fixture");
  await expect(page).toHaveURL(/q=fixture/);
  await page.getByLabel("Sort by ID").selectOption("-id");
  await expect(page).toHaveURL(/sort=-id/);
  await page.reload();
  await expect(page.getByLabel("Search assignments")).toHaveValue("fixture");
  await page.getByRole("link", { name: "fixture-owner", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "Assignment details" }),
  ).toBeVisible();
  await page.reload();
  await expect(
    page.getByText("Assignment details", { exact: true }),
  ).toBeVisible();
});
test("assignment creation uses exact identity, CSRF, idempotency and stops terminal polling", async ({
  page,
}) => {
  let polls = 0;
  await page.route("**/api/v1/operations/*", (route) => {
    polls++;
    return route.fulfill({
      json: {
        id: operationId,
        instanceId,
        kind: "commission",
        generation: 1,
        state: "Succeeded",
        attempt: 0,
        nextAttemptAt: 0,
      },
    });
  });
  await page.route("**/api/v1/assignments", async (route) => {
    if (route.request().method() !== "POST") return route.fallback();
    const request = route.request();
    expect(request.headers()["x-csrf-token"]).toBe("fixture-csrf");
    expect(request.headers()["idempotency-key"]).toMatch(/^[a-f0-9-]{36}$/);
    expect(request.postDataJSON()).toEqual({
      identity: assignment.identity,
      access: "enabled",
      templateId: "fixture-template",
      templateVersion: 1,
    });
    await route.fulfill({
      status: 202,
      json: { assignmentId: id, instanceId, operationId, version: 1 },
    });
  });
  await page.goto("/assignments");
  await page
    .getByRole("button", { name: "Create assignment", exact: true })
    .click();
  await page.getByLabel("Recorded identity").selectOption(id);
  await page.getByRole("button", { name: "Create personal instance" }).click();
  await page
    .getByRole("button", { name: "Confirm change", exact: true })
    .click();
  await expect(page.getByText("Succeeded", { exact: true })).toBeVisible();
  const terminalPolls = polls;
  await page.waitForTimeout(1200);
  expect(polls).toBe(terminalPolls);
});
test("access conflicts preserve submitted values until the refreshed version is reviewed", async ({
  page,
}) => {
  let calls = 0;
  let version = 1;
  await page.route(`**/api/v1/assignments/${id}`, async (route) => {
    if (route.request().method() === "GET")
      return route.fulfill({
        json: { ...assignment, version },
        headers: { ETag: `"${version}"` },
      });
    calls++;
    expect(route.request().postDataJSON()).toEqual({ access: "suspended" });
    expect(route.request().headers()["if-match"]).toBe(
      calls === 1 ? '"1"' : '"2"',
    );
    if (calls === 1) {
      version = 2;
      return route.fulfill({ status: 412, json: { error: "stale_version" } });
    }
    await route.fulfill({
      status: 202,
      json: { assignmentId: id, instanceId, operationId, version: 3 },
    });
  });
  await page.goto(`/assignments/${id}`);
  await page
    .getByRole("button", { name: "Change access", exact: true })
    .click();
  await page.getByLabel("Access", { exact: true }).selectOption("suspended");
  await page
    .getByRole("button", { name: "Confirm change", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Submitted values" }),
  ).toBeVisible();
  await expect(page.getByLabel("Access", { exact: true })).toHaveValue(
    "suspended",
  );
  await page
    .getByRole("button", { name: "Use refreshed version after review" })
    .click();
  await page
    .getByRole("button", { name: "Confirm change", exact: true })
    .click();
  await expect(page.getByText("Change accepted.")).toBeVisible();
});
test("runtime registration is a typed form and restores focus on close", async ({
  page,
}) => {
  await page.goto("/terminal-runtimes");
  const trigger = page.getByRole("button", {
    name: "Register runtime",
    exact: true,
  });
  await trigger.click();
  await page
    .getByLabel("Runtime: Runtime ID", { exact: true })
    .fill("python-tools");
  await page.getByLabel("Image pinned by digest").fill(definition.image);
  await page.route("**/api/v1/terminal-runtimes", (route) => {
    expect(route.request().postDataJSON().runtime.id).toBe("python-tools");
    return route.fulfill({
      status: 202,
      json: {
        eventId: operationId,
        assignmentVersion: null,
        policyGeneration: null,
        policyDigest: null,
      },
    });
  });
  await page
    .getByRole("button", { name: "Confirm change", exact: true })
    .click();
  await expect(page.getByText("Change accepted.")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(trigger).toBeFocused();
});
test("configuration JSON preserves unknown fields and blocks form switching on syntax errors", async ({
  page,
}) => {
  await page.goto("/templates/new");
  await page.getByRole("button", { name: "JSON", exact: true }).click();
  const editor = page.getByRole("textbox", {
    name: "Configuration JSON editor",
  });
  await editor.fill(JSON.stringify(definition));
  await page.getByRole("button", { name: "Form", exact: true }).click();
  await expect(
    page.getByText(/advanced/, { exact: false }).first(),
  ).toBeVisible();
  await page.getByRole("button", { name: "JSON", exact: true }).click();
  await editor.fill('{"image":');
  await expect(
    page.getByRole("button", { name: "Form", exact: true }),
  ).toBeDisabled();
  await expect(page.getByText(/Invalid JSON/).first()).toBeVisible();
});
test("lost administrator permission removes private rows and every mutation control", async ({
  page,
}) => {
  await page.goto("/assignments");
  await expect(
    page.getByRole("link", { name: "fixture-owner", exact: true }),
  ).toBeVisible();
  await page.route("**/api/v1/assignments?*", (route) =>
    route.fulfill({ status: 403, json: { error: "access_denied" } }),
  );
  await page.getByRole("button", { name: "↻ Refresh" }).click();
  await expect(page.getByRole("alert")).toContainText(
    "does not have administrator access",
  );
  await expect(
    page.getByRole("link", { name: "fixture-owner", exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Create assignment", exact: true }),
  ).toHaveCount(0);
});

test("retry, execution policy and pull-secret forms send typed references and preconditions", async ({
  page,
}) => {
  const sent: string[] = [];
  await page.route(`**/api/v1/instances/${instanceId}/reconcile`, (route) => {
    expect(route.request().headers()["if-match"]).toBe('"1"');
    expect(route.request().postDataJSON()).toEqual({});
    sent.push("retry");
    return route.fulfill({
      status: 202,
      json: { assignmentId: id, instanceId, operationId, version: 2 },
    });
  });
  await page.route(`**/api/v1/assignments/${id}/execution`, (route) => {
    if (route.request().method() === "GET") return route.fallback();
    expect(route.request().headers()["if-match"]).toBe('"1"');
    expect(route.request().postDataJSON()).toEqual({
      defaultRuntimeId: "python-tools",
      allowed: [],
    });
    sent.push("policy");
    return route.fulfill({ status: 202, json: { eventId: operationId } });
  });
  await page.route(
    `**/api/v1/instances/${instanceId}/pull-secret-bindings`,
    (route) => {
      expect(route.request().postDataJSON()).toEqual({
        instanceId,
        namespace: "fixture-instance",
        namespaceUid: "fixture-ns-uid",
        name: "registry-pull",
        uid: "fixture-secret-uid",
      });
      expect(route.request().headers()["x-csrf-token"]).toBe("fixture-csrf");
      sent.push("secret");
      return route.fulfill({ status: 202, json: { eventId: operationId } });
    },
  );
  await page.goto(`/assignments/${id}`);
  for (const name of [
    "Retry reconciliation",
    "Edit execution policy",
    "Bind pull secret",
  ]) {
    await page.getByRole("button", { name, exact: true }).click();
    const dialog = page.getByRole("dialog");
    if (name === "Bind pull secret") {
      await dialog
        .getByLabel("Namespace UID", { exact: true })
        .fill("fixture-ns-uid");
      await dialog.getByLabel("Existing secret name").fill("registry-pull");
      await dialog.getByLabel("Existing secret UID").fill("fixture-secret-uid");
    }
    await dialog
      .getByRole("button", { name: "Confirm change", exact: true })
      .click();
    await expect(dialog.getByText("Change accepted.")).toBeVisible();
    await page.keyboard.press("Escape");
  }
  expect(sent).toEqual(["retry", "policy", "secret"]);
});

test("storage registration and update preserve immutable ownership and append a version", async ({
  page,
}) => {
  const binding = {
    id,
    version: 1,
    instanceId,
    namespace: "fixture-instance",
    namespaceUid: "fixture-ns-uid",
    pvcName: "home",
    pvcUid: "fixture-pvc-uid",
    backingStorageId: "fixture-volume",
    external: false,
    required: true,
    backup: "platform_checkpoint",
    mounts: [{ path: "/workspace", readOnly: true }],
  };
  await page.route("**/api/v1/storage-bindings", (route) => {
    const created = route.request().postDataJSON();
    expect(created).toMatchObject({ ...binding, id: created.id });
    return route.fulfill({ status: 202, json: { eventId: operationId } });
  });
  await page.goto(`/storage-bindings?instanceId=${instanceId}`);
  await page
    .getByRole("button", { name: "Register storage binding", exact: true })
    .click();
  const dialog = page.getByRole("dialog");
  for (const [label, value] of [
    ["Namespace", "fixture-instance"],
    ["Namespace UID", "fixture-ns-uid"],
    ["PVC name", "home"],
    ["PVC UID", "fixture-pvc-uid"],
    ["Backing storage inventory ID", "fixture-volume"],
    ["Permitted mount 1: Approved mount path", "/workspace"],
  ])
    await dialog.getByLabel(label, { exact: true }).fill(value);
  await dialog
    .getByRole("button", { name: "Confirm change", exact: true })
    .click();
  await expect(dialog.getByText("Change accepted.")).toBeVisible();
  await page.keyboard.press("Escape");
  await page.route(`**/api/v1/storage-bindings/${id}`, (route) => {
    if (route.request().method() === "GET")
      return route.fulfill({ json: binding, headers: { ETag: '"1"' } });
    expect(route.request().headers()["if-match"]).toBe('"1"');
    expect(route.request().postDataJSON()).toEqual({
      ...binding,
      version: 2,
      required: false,
    });
    return route.fulfill({ status: 202, json: { eventId: operationId } });
  });
  await page.goto(`/storage-bindings/${id}`);
  await page
    .getByRole("button", { name: "Update storage binding", exact: true })
    .click();
  await expect(dialog.getByLabel("PVC UID", { exact: true })).toHaveAttribute(
    "readonly",
    "",
  );
  await dialog.getByLabel("Required", { exact: true }).uncheck();
  await dialog
    .getByRole("button", { name: "Confirm change", exact: true })
    .click();
  await expect(dialog.getByText("Change accepted.")).toBeVisible();
});

test("durable bulk results display independent failures and resume the original exact batch", async ({
  page,
}) => {
  const other = "00000000-0000-4000-8000-000000000004";
  let resumed = false;
  await page.route(`**/api/v1/template-batches/${id}`, (route) =>
    route.fulfill({
      json: {
        id,
        actor: { issuer, subject: "fixture-admin" },
        selection: { templateId: "fixture-template", templateVersion: 1 },
        state: resumed ? "Failed" : "Pending",
        items: [
          {
            assignmentId: id,
            expectedVersion: 1,
            state: resumed ? "Accepted" : "Pending",
            operationId: resumed ? operationId : null,
            errorCode: null,
          },
          {
            assignmentId: other,
            expectedVersion: 3,
            state: "Failed",
            operationId: null,
            errorCode: "stale_version",
          },
        ],
      },
    }),
  );
  await page.route(`**/api/v1/template-batches/${id}/resume`, (route) => {
    expect(route.request().method()).toBe("POST");
    expect(route.request().postDataJSON()).toEqual({});
    expect(route.request().headers()["idempotency-key"]).toBeTruthy();
    expect(route.request().headers()["x-csrf-token"]).toBe("fixture-csrf");
    resumed = true;
    return route.fulfill({ status: 202, json: { id, operationId: id } });
  });
  await page.goto(`/template-batches/${id}`);
  await expect(page.getByText("stale_version", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Resume pending dispatch" }).click();
  await expect(page.getByText("Accepted", { exact: true })).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Resume pending dispatch" }),
  ).toHaveCount(0);
  await page.reload();
  await expect(page.getByText("stale_version", { exact: true })).toBeVisible();
});

test("new template names are unsaved state and require explicit navigation discard", async ({
  page,
}) => {
  await page.goto("/templates/new");
  await page.getByLabel("Template ID", { exact: true }).fill("unsaved-fixture");
  await page
    .getByRole("navigation")
    .getByRole("link", { name: /Users/ })
    .click();
  await expect(
    page.getByRole("alert").filter({ hasText: "Discard unsaved changes?" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Keep editing" }).click();
  await expect(page.getByLabel("Template ID", { exact: true })).toHaveValue(
    "unsaved-fixture",
  );
  await page
    .getByRole("navigation")
    .getByRole("link", { name: /Users/ })
    .click();
  await page.getByRole("button", { name: "Discard and leave" }).click();
  await expect(page).toHaveURL(/\/users(?:\?|$)/);
});

test("bulk apply sends only explicitly selected IDs and reviewed versions", async ({
  page,
}) => {
  const input = {
    selection: { templateId: "fixture-template", templateVersion: 1 },
    members: [{ assignmentId: id, expectedVersion: 1 }],
  };
  await page.route("**/api/v1/template-batches/preview", (route) => {
    expect(route.request().postDataJSON()).toEqual(input);
    return route.fulfill({
      json: {
        items: [
          {
            assignmentId: id,
            expectedVersion: 1,
            eligible: true,
            blockedReasons: [],
            currentDefinition: definition,
            proposedDefinition: definition,
            restart: true,
          },
        ],
      },
    });
  });
  await page.route("**/api/v1/template-batches", (route) => {
    expect(route.request().postDataJSON()).toEqual(input);
    return route.fulfill({ status: 202, json: { id, operationId: id } });
  });
  await page.route(`**/api/v1/template-batches/${id}`, (route) =>
    route.fulfill({
      json: {
        id,
        actor: { issuer, subject: "fixture-admin" },
        selection: input.selection,
        state: "Succeeded",
        items: [
          {
            assignmentId: id,
            expectedVersion: 1,
            state: "Accepted",
            operationId,
            errorCode: null,
          },
        ],
      },
    }),
  );
  await page.goto("/assignments");
  await page.getByRole("checkbox", { name: "Select fixture-owner" }).check();
  await page
    .getByRole("button", { name: "Apply to 1 selected assignments" })
    .click();
  await page
    .getByLabel("Approved target version")
    .selectOption("fixture-template:1");
  await page
    .getByRole("button", { name: "Preview selected assignments" })
    .click();
  await page.getByRole("button", { name: "Apply reviewed selection" }).click();
  await expect(page.getByText("Accepted", { exact: true })).toBeVisible();
  await expect(
    page.getByRole("link", { name: "Open durable batch results" }),
  ).toHaveAttribute("href", `/template-batches/${id}`);
});

test("runtime version registration fixes the runtime identity and submits its next pinned version", async ({
  page,
}) => {
  await page.route(
    "**/api/v1/terminal-runtimes/python-tools/versions",
    (route) => {
      if (route.request().method() === "GET") return route.fallback();
      expect(route.request().postDataJSON().runtime).toEqual({
        id: "python-tools",
        version: 2,
      });
      expect(route.request().headers()["x-csrf-token"]).toBe("fixture-csrf");
      return route.fulfill({ status: 202, json: { eventId: operationId } });
    },
  );
  await page.goto("/terminal-runtimes/python-tools");
  await page
    .getByRole("button", { name: "Register runtime version", exact: true })
    .click();
  const dialog = page.getByRole("dialog");
  await expect(
    dialog.getByLabel("Runtime: Runtime ID", { exact: true }),
  ).toHaveAttribute("readonly", "");
  await dialog.getByLabel("Runtime: Version", { exact: true }).fill("2");
  await dialog.getByLabel("Image pinned by digest").fill(definition.image);
  await dialog
    .getByRole("button", { name: "Confirm change", exact: true })
    .click();
  await expect(dialog.getByText("Change accepted.")).toBeVisible();
});

test("publication conflicts require loading and reviewing the changed saved draft", async ({
  page,
}) => {
  let version = 1;
  let publishes = 0;
  const current = () => ({
    id,
    templateId: "fixture-template",
    definition: {
      ...definition,
      adminConfig: { advanced: { revision: version } },
    },
    version,
    publishedVersion: null,
  });
  await page.route(`**/api/v1/template-drafts/${id}`, (route) =>
    route.fulfill({ json: current(), headers: { ETag: `"${version}"` } }),
  );
  await page.route(`**/api/v1/template-drafts/${id}/publish`, (route) => {
    publishes++;
    if (publishes === 1) {
      expect(route.request().headers()["if-match"]).toBe('"1"');
      version = 2;
      return route.fulfill({ status: 412, json: { error: "stale_version" } });
    }
    expect(route.request().headers()["if-match"]).toBe('"2"');
    return route.fulfill({
      status: 202,
      json: {
        draft: { ...current(), version: 3, publishedVersion: 2 },
        operationId,
      },
    });
  });
  await page.goto(`/template-drafts/${id}`);
  await page
    .getByRole("button", { name: "Review publication", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Publish version", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Draft reviewed for publication" }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Review publication", exact: true }),
  ).toBeDisabled();
  await page
    .getByRole("button", {
      name: "Load current draft for a new publication review",
    })
    .click();
  expect(publishes).toBe(1);
  await page.getByRole("button", { name: "JSON", exact: true }).click();
  await expect(
    page.getByRole("textbox", { name: "Configuration JSON editor" }),
  ).toContainText('"revision": 2');
  await page
    .getByRole("button", { name: "Review publication", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Publish version", exact: true })
    .click();
  await expect(page.getByText(/Published immutable version 2/)).toBeVisible();
});
