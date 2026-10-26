import type { operations } from "../generated/api";
type Success<O> = O extends {
  responses: { 200: { content: { "application/json": infer R } } };
}
  ? R
  : O extends {
        responses: { 201: { content: { "application/json": infer R } } };
      }
    ? R
    : O extends {
          responses: { 202: { content: { "application/json": infer R } } };
        }
      ? R
      : never;
/** The success body of an operation in the administration contract. */
export type Result<T extends keyof operations> = Success<operations[T]>;
/** A request body of an operation in the administration contract. */
export type Body<T extends keyof operations> = operations[T] extends {
  requestBody: { content: { "application/json": infer B } };
}
  ? B
  : operations[T] extends {
        requestBody?: { content: { "application/json": infer B } };
      }
    ? B
    : never;
export type ConfigurationProblem = {
  pointer: string;
  code: string;
  /** The server reports errors; "warning" is a local, advisory hint. */
  severity: "error" | "warning";
  message: string;
};
const messages: Record<string, string> = {
  access_denied: "This account does not have administrator access.",
  administrator_grant_exists:
    "This identity already has a grant record. Open the existing record to update it.",
  administrators_exist:
    "Administrators already exist. Use the operator recovery command instead.",
  assignment_exists:
    "This identity already has an assignment. Open it to review its settings.",
  backend_unavailable:
    "A selected terminal backend version is not published. Choose a published version.",
  concurrent_change:
    "Another change touched the same records. Review the current state and submit again.",
  dependency_unavailable:
    "A service the manager depends on is unavailable. Try again later.",
  foreign_issuer: "This identity belongs to a different identity provider.",
  idempotency_conflict:
    "This request key was already used for a different change. Review and submit a new change.",
  identity_not_verified:
    "This identity must sign in to the manager once before it can receive a grant.",
  invalid_configuration:
    "The configuration has errors. Review the highlighted paths before submitting again.",
  invalid_identity: "The issuer or subject is not a valid identity.",
  invalid_name:
    "Names use lowercase letters, digits and hyphens, and start with a letter.",
  invalid_request:
    "Some values are invalid. Review the form before submitting again.",
  invalid_secret_value:
    "The value does not fit this secret kind. Registry secrets need a Docker config with auths.",
  last_administrator:
    "Keep at least one active administrator who can manage administrators, or use the explicit operator recovery command.",
  not_found: "This record was not found.",
  registry_unavailable: "The manager registry is unavailable. Try again later.",
  secret_in_use:
    "Templates, backends or assignments still reference this secret. Remove the references first.",
  stale_version: "The record changed. Refresh before trying again.",
  template_unavailable:
    "The selected template version is not published. Choose a published version.",
  version_not_draft:
    "Published and withdrawn versions are immutable. Create the next version instead.",
};
const problemMessages: Record<string, string> = {
  volume_overlap: "This path overlaps a volume owned by another assignment.",
  overlapping_path: "Two volumes of this assignment overlap.",
  mount_slot_unbound:
    "A required mount slot of an allowed terminal backend has no volume.",
  mount_path_mismatch:
    "A shared volume must use the same path in Hermes and in terminal backends.",
  volume_not_mounted: "Every volume must be mounted by the template.",
  unknown_storage_source:
    "This storage source is not approved for the installation.",
  dynamic_source_required:
    "This volume needs a storage class source; static sources need a path.",
  storage_migration_required:
    "Changing a volume's source or shrinking it needs an explicit migration.",
  data_identity_mismatch:
    "The backend's UID and GID must match the installation's data identity.",
  reserved_path: "This path is reserved by the platform.",
  invalid_path: "Use a normalized path without '..' segments.",
  invalid_image: "Pin images by digest: registry/name@sha256:<64 hex digits>.",
  invalid_name:
    "Names use lowercase letters, digits and hyphens, and start with a letter.",
  out_of_range: "This value is outside the supported range.",
  too_large: "This value is larger than the supported limit.",
  unknown_secret: "No admin secret has this name.",
  wrong_secret_kind: "The secret exists with a different kind.",
  one_delivery_required: "Deliver a secret as either env or file, not both.",
  duplicate_target: "Two secrets are delivered to the same target.",
  platform_enforced: "The platform enforces this setting; it cannot be set.",
  backend_not_allowed: "The default backend must be one of the allowed ones.",
  backend_unavailable: "This terminal backend version is not published.",
  template_unavailable: "This template version is not published.",
  unsupported_schema: "This schema version is not supported.",
  unsupported_contract: "Terminal backends must implement managed-ssh-v2.",
};
export function problemMessage(code: string) {
  return (
    problemMessages[code] ??
    "Review this setting against the configuration policy."
  );
}
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
          ? messages.access_denied
          : (code && messages[code]) ||
            (status === 412
              ? messages.stale_version
              : "The manager could not complete this request. Try again."),
    );
  }
}
function problemsOf(body: unknown): ConfigurationProblem[] {
  if (
    !body ||
    typeof body !== "object" ||
    !("problems" in body) ||
    !Array.isArray(body.problems)
  )
    return [];
  return body.problems
    .slice(0, 64)
    .filter(
      (value): value is { pointer: string; code: string } =>
        !!value &&
        typeof value === "object" &&
        typeof value.pointer === "string" &&
        typeof value.code === "string" &&
        /^[a-z_]{1,64}$/.test(value.code),
    )
    .map((value) => ({
      pointer: value.pointer,
      code: value.code,
      severity: "error" as const,
      message: problemMessage(value.code),
    }));
}
/** Server problems of a validation result, with display messages. */
export function displayProblems(body: unknown) {
  return problemsOf(body);
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
      problems = problemsOf(body);
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
export type Method = "POST" | "PATCH" | "PUT" | "DELETE";
/**
 * One user intent: the same method, path, body and precondition reuse one
 * idempotency key, so a retried submission is applied once.
 */
export function createIntent() {
  let previous: string | undefined;
  let key: string;
  return <T = unknown>(
    method: Method,
    path: string,
    body: unknown,
    csrf: string,
    etag?: string | null,
  ) => {
    if (etag !== undefined && (!etag || !/^"[1-9][0-9]*"$/.test(etag)))
      throw new Error("Refresh this record before editing it.");
    const serialized = body === undefined ? undefined : JSON.stringify(body);
    const fingerprint = JSON.stringify([method, path, serialized, etag]);
    if (fingerprint !== previous) {
      key = crypto.randomUUID();
      previous = fingerprint;
    }
    return request<T>(path, {
      method,
      body: serialized,
      headers: {
        ...(serialized === undefined
          ? {}
          : { "Content-Type": "application/json" }),
        "X-CSRF-Token": csrf,
        "Idempotency-Key": key,
        ...(etag ? { "If-Match": etag } : {}),
      },
    });
  };
}
