export * from "./api/transport";
import { request, versioned, type Result } from "./api/transport";
import type { components } from "./generated/api";
export type Schemas = components["schemas"];
export type Identity = Schemas["Identity"];
export type AdminSession = Result<"getSession">;
export type Assignment = Result<"getAssignment">;
export type AssignmentSpec = Schemas["AssignmentSpec"];
export type Instance = Result<"getInstance">;
export type VersionRef = Schemas["VersionRef"];
/** Catalog definitions are JSON documents checked by the local schemas. */
export type Definition = Record<string, unknown>;
export type CatalogCollection = "templates" | "terminal-backends";
export type CatalogVersion = Omit<Schemas["TemplateVersion"], "definition"> & {
  definition: Definition;
};
export const pageParams = (
  after?: string,
  extra: Record<string, string | undefined> = {},
) => {
  const params = new URLSearchParams({ limit: "50" });
  for (const [key, value] of Object.entries(extra))
    if (value !== undefined && value !== "") params.set(key, value);
  if (after) params.set("after", after);
  return params.toString();
};
const id = encodeURIComponent;
export const api = {
  session: (signal?: AbortSignal) =>
    request<AdminSession>("/api/v1/session", { signal }),
  identities: (after?: string, signal?: AbortSignal, q?: string) =>
    request<Result<"listIdentities">>(
      `/api/v1/identities?${pageParams(after, { q })}`,
      { signal },
    ),
  identity: (identityId: string, signal?: AbortSignal) =>
    request<Result<"getIdentity">>(`/api/v1/identities/${id(identityId)}`, {
      signal,
    }),
  directoryUsers: (after?: string, signal?: AbortSignal, q?: string) =>
    request<Result<"searchDirectory">>(
      `/api/v1/directory-users?${pageParams(after, { q })}`,
      { signal },
    ),
  assignments: (
    after?: string,
    signal?: AbortSignal,
    filters: { q?: string; access?: string; templateId?: string } = {},
  ) =>
    request<Result<"listAssignments">>(
      `/api/v1/assignments?${pageParams(after, filters)}`,
      { signal },
    ),
  assignment: (assignmentId: string, signal?: AbortSignal) =>
    versioned<Assignment>(`/api/v1/assignments/${id(assignmentId)}`, {
      signal,
    }),
  instance: (instanceId: string, signal?: AbortSignal) =>
    request<Instance>(`/api/v1/instances/${id(instanceId)}`, { signal }),
  environments: (instanceId: string, signal?: AbortSignal) =>
    request<Result<"listEnvironments">>(
      `/api/v1/instances/${id(instanceId)}/environments`,
      { signal },
    ),
  orphans: (signal?: AbortSignal) =>
    request<Result<"listOrphans">>("/api/v1/orphans", { signal }),
  catalog: (
    collection: CatalogCollection,
    after?: string,
    signal?: AbortSignal,
    q?: string,
  ) =>
    request<Result<"listTemplates">>(
      `/api/v1/${collection}?${pageParams(after, { q })}`,
      { signal },
    ),
  versions: (
    collection: CatalogCollection,
    catalogId: string,
    after?: string,
    signal?: AbortSignal,
  ) =>
    request<Result<"listTemplateVersions">>(
      `/api/v1/${collection}/${id(catalogId)}/versions?${pageParams(after)}`,
      { signal },
    ),
  version: (
    collection: CatalogCollection,
    catalogId: string,
    version: number,
    signal?: AbortSignal,
  ) =>
    versioned<CatalogVersion>(
      `/api/v1/${collection}/${id(catalogId)}/versions/${version}`,
      { signal },
    ),
  /** Read-only, but a POST: browser sessions still send the CSRF token. */
  validate: (
    collection: CatalogCollection,
    definition: Definition,
    csrf: string,
    signal?: AbortSignal,
  ) =>
    request<Result<"validateTemplate">>(`/api/v1/${collection}/validate`, {
      method: "POST",
      signal,
      body: JSON.stringify({ definition }),
      headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf },
    }),
  preview: (
    assignmentId: string,
    changes: Schemas["AssignmentChanges"],
    csrf: string,
    signal?: AbortSignal,
  ) =>
    request<Result<"previewAssignment">>(
      `/api/v1/assignments/${id(assignmentId)}/preview`,
      {
        method: "POST",
        signal,
        body: JSON.stringify(changes),
        headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf },
      },
    ),
  storageSources: (signal?: AbortSignal) =>
    request<Result<"listStorageSources">>("/api/v1/storage-sources", {
      signal,
    }),
  volumes: (after?: string, signal?: AbortSignal, source?: string) =>
    request<Result<"listVolumes">>(
      `/api/v1/volumes?${pageParams(after, { source })}`,
      { signal },
    ),
  secrets: (after?: string, signal?: AbortSignal) =>
    request<Result<"listSecrets">>(`/api/v1/secrets?${pageParams(after)}`, {
      signal,
    }),
  administrators: (after?: string, signal?: AbortSignal) =>
    request<Result<"listAdministrators">>(
      `/api/v1/administrators?${pageParams(after)}`,
      { signal },
    ),
  administrator: (identityId: string, signal?: AbortSignal) =>
    versioned<Result<"getAdministrator">>(
      `/api/v1/administrators/${id(identityId)}`,
      { signal },
    ),
  administratorCandidates: (after?: string, signal?: AbortSignal, q?: string) =>
    request<Result<"listAdministratorCandidates">>(
      `/api/v1/administrator-candidates?${pageParams(after, { q })}`,
      { signal },
    ),
  audit: (
    after?: string,
    signal?: AbortSignal,
    filters: { targetKind?: string; targetId?: string } = {},
  ) =>
    request<Result<"listAuditEvents">>(
      `/api/v1/audit-events?${pageParams(after, filters)}`,
      { signal },
    ),
  auditEvent: (eventId: string, signal?: AbortSignal) =>
    request<Result<"getAuditEvent">>(`/api/v1/audit-events/${id(eventId)}`, {
      signal,
    }),
  logout: (csrf: string) =>
    request<void>("/auth/logout", {
      method: "POST",
      headers: { "X-CSRF-Token": csrf },
    }),
};
/** Display label for an exact identity; labels never replace it. */
export const identityLabel = (identity: Identity) => identity.subject;
export const versionLabel = (ref: VersionRef) => `${ref.id} · v${ref.version}`;
