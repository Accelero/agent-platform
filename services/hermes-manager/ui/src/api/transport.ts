export type ConfigurationProblem = {
  pointer: string;
  code: string;
  severity: "error" | "warning";
  message: string;
};
import type { operations } from "../generated/api";
export type Result<T extends keyof operations> = operations[T] extends {
  responses: { 200: { content: { "application/json": infer R } } };
}
  ? R
  : operations[T] extends {
        responses: { 202: { content: { "application/json": infer R } } };
      }
    ? R
    : never;
const messages: Record<string, string> = {
  administrator_grant_exists:
    "This identity already has a grant record. Open the existing record to update it.",
  identity_not_verified:
    "This identity must authenticate through the manager before a dashboard grant can be created.",
  invalid_configuration:
    "The template configuration has errors. Review the highlighted paths before submitting again.",
  last_administrator:
    "Keep at least one active administrator who can manage administrators, or use the explicit operator recovery command.",
  ownership_conflict:
    "This record belongs to another management source or has conflicting lifecycle work. Review its source and current state.",
  storage_migration_required:
    "Storage-class changes and volume shrinking require an explicit migration before applying this template.",
  assignment_exists:
    "This identity already has an assignment. Open it to review its settings.",
  idempotency_conflict:
    "This request key was already used for a different change. Review and submit a new change.",
  stale_version: "The record changed. Refresh before trying again.",
  template_unavailable:
    "The selected template version is unavailable. Choose an approved version.",
  invalid_request:
    "Some values are invalid. Review the form before submitting again.",
  not_found: "This record was not found.",
  maintenance: "The manager is undergoing maintenance. Try again later.",
  registry_unavailable: "The manager registry is unavailable. Try again later.",
};
export class ApiError extends Error {
  constructor(
    public readonly status: number,
    public readonly code?: string,
    public readonly problems: ConfigurationProblem[] = [],
  ) {
    super(
      status === 401
        ? "Your session has expired. Sign in again."
        : status === 403
          ? "This account does not have administrator access."
          : (code && messages[code]) ||
            (status === 412
              ? messages.stale_version
              : "The manager could not complete this request. Try again."),
    );
  }
}
export async function versioned<T>(
  path: string,
  options: RequestInit = {},
): Promise<{ data: T; etag: string | null }> {
  const response = await fetch(path, {
    ...options,
    credentials: "same-origin",
    cache: "no-store",
    redirect: "error",
    headers: { Accept: "application/json", ...options.headers },
  });
  if (!response.ok) {
    let code: string | undefined;
    let problems: ConfigurationProblem[] = [];
    try {
      const body: unknown = await response.json();
      if (
        body &&
        typeof body === "object" &&
        "problems" in body &&
        Array.isArray(body.problems)
      )
        problems = body.problems
          .slice(0, 64)
          .filter(
            (value): value is ConfigurationProblem =>
              !!value &&
              typeof value === "object" &&
              typeof value.pointer === "string" &&
              typeof value.code === "string" &&
              ["error", "warning"].includes(value.severity),
          )
          .map((value) => ({
            ...value,
            message: "Review this setting against the configuration policy.",
          }));
      if (
        body &&
        typeof body === "object" &&
        "error" in body &&
        typeof body.error === "string" &&
        /^[a-z_]{1,80}$/.test(body.error)
      )
        code = body.error;
    } catch {
      /* Non-JSON dependency details never reach the UI. */
    }
    throw new ApiError(response.status, code, problems);
  }
  return {
    data:
      response.status === 204
        ? (undefined as T)
        : ((await response.json()) as T),
    etag: response.headers.get("ETag"),
  };
}
export async function request<T>(
  path: string,
  options: RequestInit = {},
): Promise<T> {
  return (await versioned<T>(path, options)).data;
}
export function createIntent() {
  let previous: string | undefined;
  let key: string;
  return (
    method: string,
    path: string,
    body: unknown,
    csrf: string,
    etag?: string | null,
  ) => {
    if (etag !== undefined && (!etag || !/^"[1-9][0-9]*"$/.test(etag)))
      throw new Error("Refresh this record before editing it.");
    const serialized = JSON.stringify(body);
    const fingerprint = JSON.stringify([method, path, serialized, etag]);
    if (fingerprint !== previous) {
      key = crypto.randomUUID();
      previous = fingerprint;
    }
    return request<unknown>(path, {
      method,
      body: serialized,
      headers: {
        "Content-Type": "application/json",
        "X-CSRF-Token": csrf,
        "Idempotency-Key": key,
        ...(etag ? { "If-Match": etag } : {}),
      },
    });
  };
}
