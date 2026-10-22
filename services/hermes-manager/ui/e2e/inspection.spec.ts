import { test, expect } from "@playwright/test";
const issuer = "https://sso.example.org/realms/agents";
const id = "00000000-0000-4000-8000-000000000001";
const instanceId = "00000000-0000-4000-8000-000000000002";
const operationId = "00000000-0000-4000-8000-000000000003";
const assignment = {
  id,
  instanceId,
  identity: { issuer, subject: "inspection-owner" },
  source: "manager",
  access: "enabled",
  template: { id: "inspection-template", version: 2 },
  version: 1,
  namespace: "inspection-native",
  desiredGeneration: 1,
  observedGeneration: 1,
  state: "Ready",
};
const definition = {
  schemaVersion: 1,
  image: `ghcr.io/example/hermes@sha256:${"a".repeat(64)}`,
  storageClass: "standard",
  homeGib: 8,
  workspaceGib: 16,
  cpuMillis: 1000,
  memoryMib: 1024,
  adminConfig: {
    agent: { max_turns: 24 },
    advanced: { untouched: [1, true, null] },
  },
};
const template = {
  id: "inspection-template",
  source: "manager",
  definition,
  condition: null,
  rejectedDefinition: null,
  versions: [
    { version: 2, digest: "b".repeat(64) },
    { version: 1, digest: "a".repeat(64) },
  ],
  assignedUserCount: 1,
  redactedPaths: [],
};
const runtime = {
  schemaVersion: 1,
  runtime: { id: "python-tools", version: 3 },
  image: definition.image,
  sshContract: "managed-ssh-v2",
  uid: 10001,
  gid: 10001,
  serviceAccount: "worker",
  resources: { cpuMillis: 1000, memoryMib: 1024, ephemeralStorageMib: 1024 },
  locale: "C.UTF-8",
  egress: [],
  pullSecretNames: [],
};
const operation = {
  id: operationId,
  instanceId,
  kind: "commission",
  generation: 1,
  state: "Succeeded",
  attempt: 1,
  nextAttemptAt: 0,
};
const event = {
  id,
  actor: { issuer, subject: "inspection-admin" },
  action: "assignment.set_template",
  assignmentId: id,
  entityType: null,
  entityId: null,
  operationId,
  requestId: id,
  version: 1,
  occurredAt: 1700000000,
  outcome: "Succeeded",
};
test.beforeEach(async ({ page }) => {
  await page.route("**/api/v1/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    let json: unknown = { items: [], nextCursor: null };
    if (url.pathname.endsWith("/session"))
      json = {
        actor: { issuer, subject: "inspection-admin" },
        permittedActions: [
          "assignment.create",
          "assignment.set_access",
          "instance.retry",
          "execution.policy.set",
          "template.draft.create",
          "template.draft.update",
          "template.publish",
          "assignment.set_template",
          "execution.runtime.register",
          "administrator.manage",
        ],
        csrfToken: "inspection-csrf",
      };
    else if (url.pathname === "/api/v1/identities")
      json = {
        items: [
          {
            id,
            identity: assignment.identity,
            directoryVerified: false,
            provenance: "manager_record",
            assignment,
          },
        ],
        nextCursor: null,
      };
    else if (url.pathname === `/api/v1/identities/${id}`)
      json = {
        id,
        identity: assignment.identity,
        directoryVerified: false,
        provenance: "manager_record",
        assignment,
      };
    else if (url.pathname === "/api/v1/assignments")
      json = { items: [assignment], nextCursor: null };
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
        applied: { defaultRuntimeId: "python-tools", runtimes: [] },
        digest: "a".repeat(64),
      };
    else if (url.pathname === `/api/v1/assignments/${id}/operations`)
      json = { items: [operation], nextCursor: null };
    else if (url.pathname === `/api/v1/operations/${operationId}/details`)
      json = {
        operation,
        assignmentId: id,
        steps: [
          {
            key: "native-start",
            recordedAt: 1700000000,
            verifiedAt: 1700000001,
          },
        ],
      };
    else if (url.pathname === "/api/v1/audit-events")
      json = { items: [event], nextCursor: null };
    else if (url.pathname === `/api/v1/audit-events/${id}`)
      json = {
        event,
        redacted: true,
        change: { templateId: "inspection-template", templateVersion: 2 },
      };
    else if (url.pathname === "/api/v1/template-definitions")
      json = { items: [template], nextCursor: null };
    else if (
      url.pathname === "/api/v1/template-definitions/inspection-template"
    )
      json = template;
    else if (
      url.pathname.startsWith(
        "/api/v1/template-definitions/inspection-template/versions/",
      )
    ) {
      const version = Number(url.pathname.split("/").at(-1));
      json = {
        id: template.id,
        version,
        digest: "a".repeat(64),
        definition: {
          ...definition,
          adminConfig: {
            ...definition.adminConfig,
            agent: { max_turns: version === 1 ? 12 : 24 },
          },
        },
        redactedPaths: [],
      };
    } else if (
      url.pathname === "/api/v1/templates" ||
      url.pathname === "/api/v1/template-versions"
    )
      json = {
        items: (url.searchParams.has("templateId") ? [2, 1] : [2]).map(
          (version) => ({
            id: template.id,
            version,
            image: definition.image,
            nativePort: 8642,
            nativeScheme: "http",
          }),
        ),
      };
    else if (url.pathname === "/api/v1/terminal-runtimes")
      json = { items: [runtime], nextCursor: null };
    else if (url.pathname === "/api/v1/terminal-runtimes/python-tools")
      json = runtime;
    else if (
      url.pathname === "/api/v1/terminal-runtimes/python-tools/versions" ||
      url.pathname === "/api/v1/terminal-runtimes/python-tools/version-history"
    )
      json = { items: [runtime], nextCursor: null };
    else if (
      url.pathname === "/api/v1/configuration-references/storage-classes"
    )
      json = {
        items: [{ id: "standard", source: "approved_template" }],
        nextCursor: null,
      };
    await route.fulfill({ json, headers: { ETag: '"1"' } });
  });
});
test("users expose assignment filters and navigate through detail, history and verified operations", async ({
  page,
}) => {
  await page.goto("/users");
  await expect(
    page.getByRole("cell", { name: "inspection-template · v2" }),
  ).toBeVisible();
  await page.getByLabel("Assignment", { exact: true }).selectOption("true");
  await page.getByLabel("Management source").selectOption("manager");
  await expect(page).toHaveURL(/assigned=(?:true|%22true%22)/);
  await expect(page).toHaveURL(/source=manager/);
  await page.reload();
  await page
    .getByRole("link", { name: "inspection-owner", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "User details" }),
  ).toBeVisible();
  await page.reload();
  await page
    .getByRole("link", { name: "Manage assignment and template →" })
    .click();
  await expect(
    page.getByRole("table", { name: "Assignment operations" }),
  ).toBeVisible();
  await page.getByRole("link", { name: "commission", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "Operation details" }),
  ).toBeVisible();
  await expect(page.getByRole("cell", { name: "native-start" })).toBeVisible();
  await page.reload();
  await expect(page.getByRole("cell", { name: "native-start" })).toBeVisible();
});
test("template collections search and clone immutable versions without losing advanced settings", async ({
  page,
}) => {
  let saved: unknown;
  await page.route("**/api/v1/template-drafts", (route) => {
    if (route.request().method() === "GET") return route.fallback();
    saved = route.request().postDataJSON();
    return route.fulfill({
      status: 202,
      json: {
        draft: {
          id,
          templateId: "inspection-copy",
          definition,
          version: 1,
          publishedVersion: null,
        },
        operationId,
      },
    });
  });
  await page.route(`**/api/v1/template-drafts/${id}`, (route) =>
    route.fulfill({
      json: {
        id,
        templateId: "inspection-copy",
        definition,
        version: 1,
        publishedVersion: null,
      },
      headers: { ETag: '"1"' },
    }),
  );
  await page.goto("/templates");
  await page.getByLabel("Search templates").fill("inspection");
  await expect(page).toHaveURL(/q=inspection/);
  await page.getByLabel("Management source").selectOption("manager");
  await page.getByRole("link", { name: "Clone template", exact: true }).click();
  await expect(page.getByText(/Copied configuration only/)).toBeVisible();
  await page.getByRole("button", { name: "JSON", exact: true }).click();
  await expect(
    page.getByRole("textbox", { name: "Configuration JSON editor" }),
  ).toContainText('"untouched"');
  await page.getByRole("button", { name: "Form", exact: true }).click();
  await page.getByLabel("Template ID", { exact: true }).fill("inspection-copy");
  await page.getByRole("button", { name: "Save draft", exact: true }).click();
  await expect(page).toHaveURL(/\/template-drafts\//);
  expect(saved).toEqual({ templateId: "inspection-copy", definition });
});
test("saved versions show semantic differences and can seed a next-version draft", async ({
  page,
}) => {
  await page.goto("/templates/inspection-template");
  await page.getByRole("button", { name: "Versions", exact: true }).click();
  await page.getByLabel("Compare with version").selectOption("1");
  await expect(
    page.getByRole("heading", { name: "Semantic changed paths" }),
  ).toBeVisible();
  await expect(
    page.getByText('"/adminConfig/agent/max_turns"', { exact: false }),
  ).toBeVisible();
  await page.getByRole("link", { name: "Create next version draft" }).click();
  await expect(page.getByLabel("Template ID", { exact: true })).toHaveValue(
    "inspection-template",
  );
  await expect(page.getByLabel("Template ID", { exact: true })).toBeDisabled();
});
test("runtime inspection links permitted assignments and preserves complete definitions for new versions", async ({
  page,
}) => {
  await page.goto("/terminal-runtimes/python-tools");
  await expect(
    page.getByRole("heading", {
      name: "Complete immutable runtime definition",
    }),
  ).toBeVisible();
  await expect(
    page.getByText('"ephemeralStorageMib": 1024', { exact: false }),
  ).toBeVisible();
  await page
    .getByRole("link", {
      name: "Inspect assignments allowed to request this version →",
    })
    .click();
  await expect(page).toHaveURL(/runtimeId=python-tools/);
  await expect(page).toHaveURL(/runtimeVersion=(?:3|%223%22)/);
});
test("activity opens redacted changes and related operation outcomes", async ({
  page,
}) => {
  await page.goto("/activity");
  await page
    .getByRole("link", { name: "assignment.set_template", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Redacted change" }),
  ).toBeVisible();
  await expect(
    page.getByText('"templateVersion": 2', { exact: false }),
  ).toBeVisible();
  await page.getByRole("link", { name: "Inspect operation →" }).click();
  await expect(
    page.getByRole("heading", { name: "Operation status" }),
  ).toBeVisible();
});
test("raw editor obtains runtime value completion from the authorized catalog", async ({
  page,
}) => {
  await page.goto("/templates/new");
  await page.getByRole("button", { name: "JSON", exact: true }).click();
  const editor = page.getByRole("textbox", {
    name: "Configuration JSON editor",
  });
  await editor.fill('{"adminConfig":{"terminal":{"runtime_id":"py');
  await editor.press("Control+Space");
  await expect(page.locator(".cm-tooltip-autocomplete")).toContainText(
    "python-tools",
  );
  await editor.press("Enter");
  await expect(editor).toContainText('"python-tools"');
});

test("draft collections retain publication filters and clone without reusing record identity", async ({
  page,
}) => {
  const draft = {
    id,
    templateId: "inspection-template",
    definition,
    version: 4,
    publishedVersion: 2,
  };
  await page.route("**/api/v1/template-drafts?*", (route) =>
    route.fulfill({ json: { items: [draft], nextCursor: null } }),
  );
  await page.route(`**/api/v1/template-drafts/${id}`, (route) =>
    route.fulfill({ json: draft, headers: { ETag: '"4"' } }),
  );
  await page.goto("/templates");
  await page.getByRole("button", { name: "Drafts", exact: true }).click();
  await page.getByLabel("Publication state").selectOption("published");
  await page.getByLabel("Search drafts").fill("inspection");
  await expect(page).toHaveURL(/publication=published/);
  await page.reload();
  await expect(
    page.getByRole("table", { name: "Manager-owned drafts" }),
  ).toBeVisible();
  await page.getByRole("link", { name: "Clone draft", exact: true }).click();
  await expect(page.getByText(/Copied configuration only/)).toBeVisible();
  await expect(page.getByLabel("Template ID", { exact: true })).toHaveValue("");
  await page.getByRole("button", { name: "JSON", exact: true }).click();
  await expect(
    page.getByRole("textbox", { name: "Configuration JSON editor" }),
  ).toContainText('"untouched"');
});
test("execution binding suggestions use the exact owner and fill their approved version", async ({
  page,
}) => {
  await page.route(
    "**/api/v1/configuration-references/storage-bindings?*",
    (route) => {
      const url = new URL(route.request().url());
      expect(url.searchParams.get("instanceId")).toBe(instanceId);
      return route.fulfill({
        json: {
          items: [
            {
              id,
              version: 2,
              instanceId,
              namespace: "inspection-native",
              namespaceUid: "fixture-namespace",
              pvcName: "network-home",
              pvcUid: "fixture-pvc",
              backingStorageId: id,
              external: true,
              required: true,
              backup: "external",
              mounts: [{ path: "/workspace", readOnly: true }],
            },
          ],
          nextCursor: null,
        },
      });
    },
  );
  await page.goto(`/assignments/${id}`);
  await page
    .getByRole("button", { name: "Edit execution policy", exact: true })
    .click();
  const dialog = page.getByRole("dialog");
  await dialog
    .getByRole("button", { name: "Add Allowed runtime", exact: true })
    .click();
  await dialog
    .getByRole("button", { name: "Add Mount selection", exact: true })
    .click();
  await expect(dialog.locator(`datalist option[value="${id}"]`)).toHaveCount(1);
  await dialog
    .getByLabel("Allowed runtime 1: Mount selection 1: Binding ID", {
      exact: true,
    })
    .fill(id);
  await expect(
    dialog.getByLabel("Allowed runtime 1: Mount selection 1: Binding version", {
      exact: true,
    }),
  ).toHaveValue("2");
});

test("failed refresh distinguishes stale data from an empty collection", async ({
  page,
}) => {
  let unavailable = false;
  await page.route("**/api/v1/identities?*", (route) =>
    unavailable
      ? route.fulfill({
          status: 503,
          json: { error: "dependency_unavailable" },
        })
      : route.fallback(),
  );
  await page.goto("/users");
  await expect(
    page.getByRole("link", { name: "inspection-owner", exact: true }),
  ).toBeVisible();
  unavailable = true;
  await page.getByRole("button", { name: "↻ Refresh", exact: true }).click();
  await expect(
    page.getByRole("status").filter({
      hasText: "Previously loaded information could not be refreshed",
    }),
  ).toBeVisible();
  await expect(
    page.getByRole("link", { name: "inspection-owner", exact: true }),
  ).toHaveCount(0);
  await expect(page.getByText("No identities are recorded yet.")).toHaveCount(
    0,
  );
});

test("administrator lookup searches and pages verified identities beyond the initial page", async ({
  page,
}) => {
  const searches: string[] = [];
  await page.route("**/api/v1/administrator-identities?*", async (route) => {
    const url = new URL(route.request().url());
    searches.push(url.search);
    await route.fulfill({
      json: {
        items: [
          {
            issuer,
            subject: url.searchParams.has("after")
              ? "verified-next"
              : "verified-first",
          },
        ],
        nextCursor: url.searchParams.has("after") ? null : "verified-page-two",
      },
    });
  });
  await page.goto("/administrators");
  await page.getByLabel("Search verified identities").fill("verified");
  await expect(
    page
      .getByLabel("Verified identity")
      .getByRole("option", { name: /verified-first/ }),
  ).toHaveCount(1);
  await page.getByRole("button", { name: "More identities" }).click();
  await expect(
    page
      .getByLabel("Verified identity")
      .getByRole("option", { name: /verified-next/ }),
  ).toHaveCount(1);
  await page
    .getByRole("combobox", { name: "Verified identity", exact: true })
    .selectOption({ label: `verified-next · ${issuer}` });
  await expect(
    page.getByRole("button", { name: "Grant administrator access" }),
  ).toBeVisible();
  expect(
    searches.some(
      (query) =>
        query.includes("q=verified") &&
        query.includes("after=verified-page-two"),
    ),
  ).toBeTruthy();
});
