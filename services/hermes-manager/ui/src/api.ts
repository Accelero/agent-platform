export * from "./api/transport";
import { request, versioned, type Result } from "./api/transport";
export type AdminSession = Result<"getAdministrationSession">;
export type Assignments = Result<"listAssignments">;
export type Runtimes = Result<"listTerminalRuntimes">;
export type Audit = Result<"listAuditEvents">;
export const pageParams = (after?: string | number, extra = {}) => {
  const params = new URLSearchParams({ limit: "50", ...extra });
  if (after !== undefined && after !== "") params.set("after", String(after));
  return params.toString();
};
export const api = {
  directoryUsers: (after?: string, signal?: AbortSignal, q = "") =>
    request<Result<"searchDirectoryUsers">>(
      `/api/v1/directory-users?${pageParams(after, { q })}`,
      { signal },
    ),
  templateObservation: (id: string, signal?: AbortSignal) =>
    request<Result<"getAssignmentTemplateObservation">>(
      `/api/v1/assignments/${encodeURIComponent(id)}/template-observation`,
      { signal },
    ),
  runtimeHistory: (
    id: string,
    after?: string,
    signal?: AbortSignal,
    filters: Record<string, string> = {},
  ) =>
    request<Result<"listRuntimeVersionHistory">>(
      `/api/v1/terminal-runtimes/${encodeURIComponent(id)}/version-history?${pageParams(after, filters)}`,
      { signal },
    ),
  runtime: (id: string, signal?: AbortSignal) =>
    request<Result<"getTerminalRuntime">>(
      `/api/v1/terminal-runtimes/${encodeURIComponent(id)}`,
      { signal },
    ),
  bindingReferences: (
    instanceId: string,
    q: string,
    signal?: AbortSignal,
    after?: string,
    filters: Record<string, string> = {},
  ) =>
    request<Result<"listStorageBindingReferences">>(
      `/api/v1/configuration-references/storage-bindings?${pageParams(after, { ...filters, instanceId, q })}`,
      { signal },
    ),
  identity: (id: string, signal?: AbortSignal) =>
    request<Result<"getIdentity">>(
      `/api/v1/identities/${encodeURIComponent(id)}`,
      { signal },
    ),
  operationDetails: (id: string, signal?: AbortSignal) =>
    request<Result<"getOperationDetails">>(
      `/api/v1/operations/${encodeURIComponent(id)}/details`,
      { signal },
    ),
  auditDetails: (id: string, signal?: AbortSignal) =>
    request<Result<"getAuditDetails">>(
      `/api/v1/audit-events/${encodeURIComponent(id)}`,
      { signal },
    ),
  operationHistory: (
    id: string,
    after?: string,
    signal?: AbortSignal,
    filters: Record<string, string> = {},
  ) =>
    request<Result<"listAssignmentOperations">>(
      `/api/v1/assignments/${encodeURIComponent(id)}/operations?${pageParams(after, filters)}`,
      { signal },
    ),
  templateVersion: (id: string, version: number, signal?: AbortSignal) =>
    request<Result<"getTemplateVersionDefinition">>(
      `/api/v1/template-definitions/${encodeURIComponent(id)}/versions/${version}`,
      { signal },
    ),
  storageClasses: (q: string, signal?: AbortSignal) =>
    request<Result<"listApprovedStorageClasses">>(
      `/api/v1/configuration-references/storage-classes?${pageParams(undefined, { q })}`,
      { signal },
    ),
  templatePreview: (
    id: string,
    selection: { templateId: string; templateVersion: number },
    signal?: AbortSignal,
  ) =>
    request<Result<"previewAssignmentTemplate">>(
      `/api/v1/assignments/${encodeURIComponent(id)}/template-preview?${new URLSearchParams({ templateId: selection.templateId, templateVersion: String(selection.templateVersion) })}`,
      { signal },
    ),
  templateBatch: (id: string, signal?: AbortSignal) =>
    request<Result<"getTemplateBatch">>(
      `/api/v1/template-batches/${encodeURIComponent(id)}`,
      { signal },
    ),
  administrators: (
    after?: string,
    signal?: AbortSignal,
    filters: Record<string, string> = {},
  ) =>
    request<Result<"listAdministrators">>(
      `/api/v1/administrators?${pageParams(after, filters)}`,
      { signal },
    ),
  administrator: (id: string, signal?: AbortSignal) =>
    versioned<Result<"getAdministratorGrant">>(
      `/api/v1/administrators/${encodeURIComponent(id)}`,
      { signal },
    ),
  administratorCandidates: (signal?: AbortSignal, after?: string, q = "") =>
    request<Result<"listAdministratorCandidates">>(
      `/api/v1/administrator-identities?${pageParams(after, { q })}`,
      { signal },
    ),
  schemas: (signal?: AbortSignal) =>
    request<Result<"getConfigurationSchemas">>(
      "/api/v1/configuration-schemas",
      { signal },
    ),
  session: (signal?: AbortSignal) =>
    request<AdminSession>("/api/v1/session", { signal }),
  assignments: (
    after?: string,
    signal?: AbortSignal,
    filters: Record<string, string> = {},
  ) =>
    request<Assignments>(`/api/v1/assignments?${pageParams(after, filters)}`, {
      signal,
    }),
  assignment: (id: string, signal?: AbortSignal) =>
    versioned<Result<"getAssignment">>(
      `/api/v1/assignments/${encodeURIComponent(id)}`,
      { signal },
    ),
  instance: (id: string, signal?: AbortSignal) =>
    versioned<Result<"getInstance">>(
      `/api/v1/instances/${encodeURIComponent(id)}`,
      { signal },
    ),
  execution: (id: string, signal?: AbortSignal) =>
    versioned<Result<"getAssignmentExecution">>(
      `/api/v1/assignments/${encodeURIComponent(id)}/execution`,
      { signal },
    ),
  runtimes: (
    after?: string,
    signal?: AbortSignal,
    filters: Record<string, string> = {},
  ) =>
    request<Runtimes>(
      `/api/v1/terminal-runtimes?${pageParams(after, filters)}`,
      {
        signal,
      },
    ),
  runtimeVersions: (id: string, after?: number, signal?: AbortSignal) =>
    request<Result<"listTerminalRuntimeVersions">>(
      `/api/v1/terminal-runtimes/${encodeURIComponent(id)}/versions?${pageParams(after)}`,
      { signal },
    ),
  audit: (
    after?: string,
    signal?: AbortSignal,
    filters: Record<string, string> = {},
  ) =>
    request<Audit>(`/api/v1/audit-events?${pageParams(after, filters)}`, {
      signal,
    }),
  identities: (
    after?: string,
    signal?: AbortSignal,
    filters: Record<string, string> = {},
  ) =>
    request<Result<"listIdentities">>(
      `/api/v1/identities?${pageParams(after, filters)}`,
      { signal },
    ),
  drafts: (
    signal?: AbortSignal,
    after?: string,
    filters: Record<string, string> = {},
  ) =>
    request<Result<"listTemplateDrafts">>(
      `/api/v1/template-drafts?${pageParams(after, filters)}`,
      {
        signal,
      },
    ),
  draft: (id: string, signal?: AbortSignal) =>
    versioned<Result<"getTemplateDraft">>(
      `/api/v1/template-drafts/${encodeURIComponent(id)}`,
      { signal },
    ),
  templateDefinitions: (
    signal?: AbortSignal,
    after?: string,
    filters: Record<string, string> = {},
  ) =>
    request<Result<"listTemplateDefinitions">>(
      `/api/v1/template-definitions?${pageParams(after, filters)}`,
      {
        signal,
      },
    ),
  templateDefinition: (id: string, signal?: AbortSignal) =>
    request<Result<"getTemplateDefinition">>(
      `/api/v1/template-definitions/${encodeURIComponent(id)}`,
      { signal },
    ),
  templates: (
    signal?: AbortSignal,
    after?: string,
    filters: Record<string, string> = {},
  ) =>
    request<Result<"listTemplateVersions">>(
      `/api/v1/template-versions?${pageParams(after, filters)}`,
      { signal },
    ),
  storage: (instanceId: string, after?: string, signal?: AbortSignal) =>
    request<Result<"listOwnerStorageBindings">>(
      `/api/v1/storage-bindings?${pageParams(after, { instanceId })}`,
      { signal },
    ),
  storageBinding: (id: string, signal?: AbortSignal) =>
    versioned<Result<"getOwnerStorageBinding">>(
      `/api/v1/storage-bindings/${encodeURIComponent(id)}`,
      { signal },
    ),
  operation: (id: string, signal?: AbortSignal) =>
    request<Result<"getOperation">>(
      `/api/v1/operations/${encodeURIComponent(id)}`,
      { signal },
    ),
  logout: (csrf: string) =>
    request<void>("/auth/logout", {
      method: "POST",
      headers: { "X-CSRF-Token": csrf },
    }),
};
