import type { operations } from "./generated/api";

type Result<T extends keyof operations> = operations[T] extends {
  responses: { 200: { content: { "application/json": infer R } } };
}
  ? R
  : never;

export type AdminSession = Result<"getAdministrationSession">;
export type Assignments = Result<"listAssignments">;
export type Runtimes = Result<"listTerminalRuntimes">;
export type Audit = Result<"listAuditEvents">;

export class ApiError extends Error {
  constructor(public readonly status: number) {
    super(
      status === 401
        ? "Your session has expired. Sign in again."
        : status === 403
          ? "This account does not have administrator access."
          : status === 409 || status === 412
            ? "The record changed. Refresh before trying again."
            : "The manager could not complete this request. Try again.",
    );
  }
}

async function request<T>(path: string, options: RequestInit = {}): Promise<T> {
  const response = await fetch(path, {
    ...options,
    credentials: "same-origin",
    cache: "no-store",
    redirect: "error",
    headers: { Accept: "application/json", ...options.headers },
  });
  if (!response.ok) throw new ApiError(response.status);
  return response.status === 204
    ? (undefined as T)
    : (response.json() as Promise<T>);
}

export const api = {
  session: () => request<AdminSession>("/api/v1/session"),
  assignments: () => request<Assignments>("/api/v1/assignments?limit=50"),
  runtimes: () => request<Runtimes>("/api/v1/terminal-runtimes?limit=50"),
  audit: () => request<Audit>("/api/v1/audit-events?limit=50"),
  logout: (csrf: string) =>
    request<void>("/auth/logout", {
      method: "POST",
      headers: { "X-CSRF-Token": csrf },
    }),
};
