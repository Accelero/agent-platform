import { afterEach, describe, expect, it, vi } from "vitest";
import { QueryClient } from "@tanstack/react-query";
import { ApiError, createIntent } from "../api";
import { clearPrivateState, queryClient, retryRead } from "./state";
afterEach(() => {
  vi.unstubAllGlobals();
  queryClient.clear();
});
describe("private state and retry policy", () => {
  it("clears cached data and cancels outstanding reads", async () => {
    const client = new QueryClient();
    let aborted = false;
    client.setQueryData(["private"], { subject: "fixture-admin" });
    const pending = client
      .fetchQuery({
        queryKey: ["pending"],
        queryFn: ({ signal }) =>
          new Promise((resolve) => {
            signal.addEventListener("abort", () => {
              aborted = true;
              resolve(null);
            });
          }),
      })
      .catch(() => {});
    clearPrivateState(client);
    await pending;
    expect(aborted).toBe(true);
    expect(client.getQueryCache().getAll()).toEqual([]);
  });
  it("clears every cache and notifies authorization loss on 401", async () => {
    vi.stubGlobal("window", {
      location: { pathname: "/assignments", search: "", replace: vi.fn() },
    });
    queryClient.setQueryData(["private"], { subject: "fixture-admin" });
    await expect(
      queryClient.fetchQuery({
        queryKey: ["expired"],
        queryFn: () => {
          throw new ApiError(401);
        },
      }),
    ).rejects.toThrow("expired");
    expect(queryClient.getQueryCache().getAll()).toEqual([]);
    expect(window.location.replace).toHaveBeenCalledWith(
      "/auth/login?returnTo=%2Fassignments",
    );
  });
  it("retries only bounded transient read failures", () => {
    for (const status of [400, 401, 403, 404, 409, 412, 422, 500])
      expect(retryRead(0, new ApiError(status))).toBe(false);
    expect(retryRead(0, new ApiError(503))).toBe(true);
    expect(retryRead(1, new TypeError("network"))).toBe(true);
    expect(retryRead(2, new ApiError(503))).toBe(false);
  });
});
describe("request intent", () => {
  it("reuses keys for identical retries, rotates on edited body or precondition", async () => {
    const fetch = vi
      .fn()
      .mockImplementation(() =>
        Promise.resolve(new Response("{}", { status: 202 })),
      );
    vi.stubGlobal("fetch", fetch);
    const submit = createIntent();
    const body = { access: "suspended" };
    await submit("PATCH", "/api/v1/assignments/fixture", body, "csrf", '"1"');
    await submit("PATCH", "/api/v1/assignments/fixture", body, "csrf", '"1"');
    await submit(
      "PATCH",
      "/api/v1/assignments/fixture",
      { access: "enabled" },
      "csrf",
      '"1"',
    );
    await submit(
      "PATCH",
      "/api/v1/assignments/fixture",
      { access: "enabled" },
      "csrf",
      '"2"',
    );
    const headers = fetch.mock.calls.map((call) => call[1].headers);
    expect(headers[0]["Idempotency-Key"]).toBe(headers[1]["Idempotency-Key"]);
    expect(headers[1]["Idempotency-Key"]).not.toBe(
      headers[2]["Idempotency-Key"],
    );
    expect(headers[2]["Idempotency-Key"]).not.toBe(
      headers[3]["Idempotency-Key"],
    );
    expect(headers[0]["If-Match"]).toBe('"1"');
    expect(headers[0]["X-CSRF-Token"]).toBe("csrf");
    expect(() => submit("PATCH", "/fixture", {}, "csrf", 'W/"1"')).toThrow(
      "Refresh",
    );
  });
  it("retains safe API codes and maps them without reflecting response detail", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(
          JSON.stringify({
            error: "assignment_exists",
            detail: "private text",
          }),
          { status: 409 },
        ),
      ),
    );
    await expect(
      createIntent()("POST", "/api/v1/assignments", {}, "csrf"),
    ).rejects.toMatchObject({
      status: 409,
      code: "assignment_exists",
      message: expect.stringContaining("already has an assignment"),
    });
  });
});
