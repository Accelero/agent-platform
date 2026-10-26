import type { Page, Request } from "@playwright/test";

// Presentation fixtures only. Real HTTPS OIDC sessions, PostgreSQL grants and
// contract conformance are proven by the Rust `hermes-http` tests.
export const ISSUER = "https://sso.example.org/realms/agents";
export const identity = (subject: string) => ({ issuer: ISSUER, subject });
export const IMAGE = `ghcr.io/example/hermes@sha256:${"a".repeat(64)}`;
export const ASSIGNMENT = "6f1c2a52-8f0e-4d55-9a4b-0d8c1b2e3f40";
export const INSTANCE = "0b7d4c1e-2a3f-4e5d-8c6b-7a9f0e1d2c3b";
export const IDENTITY = "a3c1e2f4-5b6d-4e7f-8a9b-0c1d2e3f4a5b";

export type Recorded = {
  method: string;
  path: string;
  body: unknown;
  headers: Record<string, string>;
};
export const template = {
  schemaVersion: 2,
  image: IMAGE,
  resources: { cpuMillis: 1000, memoryMib: 2048 },
  home: { source: "nvme", sizeGib: 5 },
  mounts: { workspace: { mountPath: "/workspace", readOnly: false } },
  defaultVolumes: {
    workspace: { source: "nvme", sizeGib: 10, readOnly: false },
  },
  adminConfig: { model: { provider: "custom" } },
  secrets: [{ name: "llm-key", env: "OPENAI_API_KEY" }],
};
const instance = {
  id: INSTANCE,
  assignmentId: ASSIGNMENT,
  namespace: "hi-0123456789ab-0b7d4c1e",
  desiredGeneration: 2,
  observedGeneration: 2,
  executionGeneration: 1,
  phase: "ready",
  blockedReason: null,
  conditions: [{ type: "storage", status: "true", reason: "bound" }],
  observedAt: "2026-10-22T08:00:00Z",
};
const assignment = {
  id: ASSIGNMENT,
  identityId: IDENTITY,
  identity: identity("bob"),
  spec: {
    access: "enabled",
    template: { id: "standard", version: 1 },
    volumes: {
      workspace: {
        source: "host-data",
        path: "bob/workspace",
        readOnly: false,
      },
    },
    terminal: { mode: "per_session", default: "ssh", allowed: { ssh: 1 } },
  },
  version: 3,
  instance,
};
const page = <T>(items: T[]) => ({ items, nextCursor: null });

/** Serves a small administration API and records every mutation. */
export async function mockApi(
  pageObject: Page,
  options: { permitted?: string[]; directory?: boolean } = {},
) {
  const recorded: Recorded[] = [];
  const permitted = options.permitted ?? ["administer", "administrator.manage"];
  const respond = (request: Request) => {
    const url = new URL(request.url());
    const path = url.pathname;
    const method = request.method();
    if (method !== "GET")
      recorded.push({
        method,
        path,
        body: request.postData() ? JSON.parse(request.postData()!) : undefined,
        headers: request.headers(),
      });
    if (path === "/api/v1/session")
      return {
        json: {
          actor: identity("fixture-admin"),
          permittedActions: permitted,
          csrfToken: "fixture-csrf",
        },
      };
    if (path === "/api/v1/identities")
      return {
        json: page([
          {
            id: IDENTITY,
            identity: identity("bob"),
            displayName: "Bob Example",
            email: "bob@example.org",
            verified: true,
            assignmentId: ASSIGNMENT,
            administrator: false,
          },
        ]),
      };
    if (path === "/api/v1/directory-users")
      return {
        json:
          options.directory === false
            ? { enabled: false, items: [], nextCursor: null }
            : {
                enabled: true,
                items: [
                  {
                    identity: identity("carol"),
                    username: "carol",
                    displayName: "Carol Example",
                    email: "carol@example.org",
                    enabled: true,
                  },
                ],
                nextCursor: null,
              },
      };
    if (path === "/api/v1/assignments" && method === "GET")
      return {
        json: page([
          {
            id: ASSIGNMENT,
            identity: identity("bob"),
            access: "enabled",
            template: { id: "standard", version: 1 },
            version: 3,
            instanceId: INSTANCE,
            phase: "ready",
          },
        ]),
      };
    if (path === "/api/v1/assignments" && method === "POST")
      return {
        status: 202,
        json: {
          assignmentId: "11111111-2222-4333-8444-555555555555",
          instanceId: "66666666-7777-4888-8999-000000000000",
          version: 1,
        },
      };
    if (path === `/api/v1/assignments/${ASSIGNMENT}` && method === "GET")
      return { json: assignment, headers: { ETag: '"3"' } };
    if (path === `/api/v1/assignments/${ASSIGNMENT}/preview`)
      return {
        json: {
          valid: true,
          problems: [],
          restartsHermes: true,
          recreatesTerminals: false,
        },
      };
    if (path === `/api/v1/assignments/${ASSIGNMENT}` && method === "PATCH")
      return {
        status: 202,
        json: { assignmentId: ASSIGNMENT, instanceId: INSTANCE, version: 4 },
      };
    if (path === "/api/v1/assignment-rollouts")
      return {
        json: {
          results: [
            {
              assignmentId: ASSIGNMENT,
              outcome: "accepted",
              receipt: {
                assignmentId: ASSIGNMENT,
                instanceId: INSTANCE,
                version: 4,
              },
              error: null,
            },
          ],
        },
      };
    if (path === "/api/v1/templates" || path === "/api/v1/terminal-backends")
      return {
        json: page([
          {
            id: path.endsWith("templates") ? "standard" : "ssh",
            versions: 2,
            latestPublished: 1,
            draft: 2,
            assignments: 1,
          },
        ]),
      };
    if (
      /^\/api\/v1\/(templates|terminal-backends)\/[a-z-]+\/versions$/.test(path)
    ) {
      if (method === "POST")
        return {
          status: 201,
          json: { id: "standard", version: 3, revision: 1 },
        };
      return {
        json: page([
          {
            id: "standard",
            version: 2,
            state: "draft",
            revision: 4,
            updatedAt: "2026-10-22T08:00:00Z",
            assignments: 0,
          },
          {
            id: "standard",
            version: 1,
            state: "published",
            revision: 3,
            updatedAt: "2026-10-21T08:00:00Z",
            assignments: 1,
          },
        ]),
      };
    }
    const version = path.match(
      /^\/api\/v1\/(templates|terminal-backends)\/([a-z-]+)\/versions\/(\d+)(\/publish|\/withdraw)?$/,
    );
    if (version) {
      if (version[4] || method === "PATCH")
        return {
          json: { id: version[2], version: Number(version[3]), revision: 5 },
        };
      const draft = version[3] === "2";
      return {
        json: {
          id: version[2],
          version: Number(version[3]),
          state: draft ? "draft" : "published",
          revision: draft ? 4 : 3,
          updatedAt: "2026-10-22T08:00:00Z",
          assignments: draft ? 0 : 1,
          definition: template,
        },
        headers: { ETag: draft ? '"4"' : '"3"' },
      };
    }
    if (path.endsWith("/validate"))
      return {
        json: {
          valid: false,
          problems: [
            {
              pointer: "/secrets/0/name",
              code: "unknown_secret",
              severity: "error",
            },
          ],
        },
      };
    if (path === "/api/v1/storage-sources")
      return {
        json: {
          items: [
            {
              id: "host-data",
              localPath: { node: "server-1", root: "/srv/hermes" },
            },
            { id: "nvme", storageClass: { name: "local-nvme" } },
          ],
          dataIdentity: { uid: 1000, gid: 1000 },
        },
      };
    if (path === "/api/v1/volumes")
      return {
        json: page([
          {
            assignmentId: ASSIGNMENT,
            identity: identity("bob"),
            name: "workspace",
            source: "host-data",
            path: "bob/workspace",
            sizeGib: null,
            readOnly: false,
          },
        ]),
      };
    if (path === `/api/v1/instances/${INSTANCE}/environments`)
      return {
        json: {
          items: [
            {
              id: "5a5a5a5a-1b1b-4c2c-8d3d-4e4e4e4e4e4e",
              backend: { id: "ssh", version: 1 },
              mode: "per_session",
              session: "s1",
              generation: 2,
              state: "draining",
              failure: "image_unavailable",
              leases: 0,
              createdAt: "2026-10-22T08:00:00Z",
            },
          ],
        },
      };
    if (path === "/api/v1/orphans")
      return {
        json: {
          scannedAt: "2026-10-22T08:05:00Z",
          items: [
            {
              kind: "namespace",
              name: "hermes-stray",
              uid: "4f4f4f4f-0000-4000-8000-000000000001",
              firstSeenAt: "2026-10-22T08:00:00Z",
              lastSeenAt: "2026-10-22T08:05:00Z",
            },
          ],
        },
      };
    if (path === "/api/v1/secrets")
      return {
        json: page([
          {
            name: "llm-key",
            kind: "env",
            version: 2,
            updatedAt: "2026-10-22T08:00:00Z",
            references: 1,
          },
        ]),
      };
    if (path.startsWith("/api/v1/secrets/"))
      return { json: { name: path.split("/").at(-1), version: 3 } };
    if (path === "/api/v1/administrators")
      return {
        json: page([
          {
            identityId: IDENTITY,
            identity: identity("fixture-admin"),
            active: true,
            canManageAdministrators: true,
            version: 1,
            updatedAt: "2026-10-22T08:00:00Z",
          },
        ]),
      };
    if (path === "/api/v1/administrator-candidates") return { json: page([]) };
    if (path === "/api/v1/audit-events")
      return {
        json: page([
          {
            id: "99999999-8888-4777-8666-555555555555",
            occurredAt: "2026-10-22T08:00:00Z",
            actor: {
              kind: "administrator",
              identity: identity("fixture-admin"),
            },
            action: "assignment.update",
            target: { kind: "assignment", id: ASSIGNMENT },
            requestId: "12121212-3434-4565-8787-909090909090",
            change: { access: "enabled" },
          },
        ]),
      };
    return { status: 404, json: { error: "not_found" } };
  };
  await pageObject.route("**/api/v1/**", async (route) => {
    const { status, json, headers } = respond(route.request()) as {
      status?: number;
      json: unknown;
      headers?: Record<string, string>;
    };
    await route.fulfill({ status: status ?? 200, json, headers });
  });
  return recorded;
}
