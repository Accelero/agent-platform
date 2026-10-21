import { afterEach, describe, expect, it, vi } from "vitest";
import { api, ApiError } from "./api";

afterEach(() => vi.unstubAllGlobals());

describe("administration client", () => {
  it("uses same-origin opaque sessions without browser bearer tokens", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValue(
        new Response(JSON.stringify({ actor: { subject: "fixture" } })),
      );
    vi.stubGlobal("fetch", fetch);
    await api.session();
    expect(fetch).toHaveBeenCalledWith(
      "/api/v1/session",
      expect.objectContaining({
        credentials: "same-origin",
        cache: "no-store",
        redirect: "error",
        headers: { Accept: "application/json" },
      }),
    );
  });
  it.each([401, 403, 409, 412, 503])(
    "preserves status %s and redacts server response bodies",
    async (status) => {
      vi.stubGlobal(
        "fetch",
        vi
          .fn()
          .mockResolvedValue(
            new Response("private dependency data", { status }),
          ),
      );
      try {
        await api.session();
        throw new Error("Expected API failure");
      } catch (error) {
        expect(error).toBeInstanceOf(ApiError);
        expect((error as ApiError).status).toBe(status);
        expect((error as Error).message).not.toContain("private dependency");
      }
    },
  );
  it("logs out through the CSRF-protected POST endpoint", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValue(new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetch);
    await api.logout("fixture-csrf");
    expect(fetch).toHaveBeenCalledWith(
      "/auth/logout",
      expect.objectContaining({
        method: "POST",
        headers: { Accept: "application/json", "X-CSRF-Token": "fixture-csrf" },
      }),
    );
  });
});
