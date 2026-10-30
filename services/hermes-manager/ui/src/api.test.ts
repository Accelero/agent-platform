import { afterEach, describe, expect, it, vi } from "vitest";
import { api, ApiError, createIntent } from "./api";

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
  it("reuses one idempotency key per intent and a new one per change", async () => {
    const fetch = vi
      .fn()
      .mockImplementation(() =>
        Promise.resolve(new Response(JSON.stringify({ version: 2 }))),
      );
    vi.stubGlobal("fetch", fetch);
    const submit = createIntent();
    await submit(
      "PATCH",
      "/api/v1/assignments/a",
      { access: "suspended" },
      "c",
      '"1"',
    );
    await submit(
      "PATCH",
      "/api/v1/assignments/a",
      { access: "suspended" },
      "c",
      '"1"',
    );
    await submit(
      "PATCH",
      "/api/v1/assignments/a",
      { access: "revoked" },
      "c",
      '"1"',
    );
    const keys = fetch.mock.calls.map(
      ([, options]) =>
        (options.headers as Record<string, string>)["Idempotency-Key"],
    );
    expect(keys[0]).toBe(keys[1]);
    expect(keys[2]).not.toBe(keys[0]);
    expect(fetch.mock.calls[0][1].headers["If-Match"]).toBe('"1"');
    expect(fetch.mock.calls[0][1].headers["Content-Type"]).toBe(
      "application/json",
    );
  });
  it("sends body-less commands without a body or content type", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValue(new Response(JSON.stringify({ revision: 3 })));
    vi.stubGlobal("fetch", fetch);
    await createIntent()(
      "POST",
      "/api/v1/templates/t/versions/1/publish",
      undefined,
      "c",
      '"2"',
    );
    const [, options] = fetch.mock.calls[0];
    expect(options.body).toBeUndefined();
    expect(options.headers["Content-Type"]).toBeUndefined();
  });
  it("refuses changes without a usable ETag", () => {
    expect(() =>
      createIntent()("PATCH", "/api/v1/assignments/a", {}, "c", null),
    ).toThrow("Refresh");
  });
  it("keeps only well-formed problems and never echoes server text", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(
          JSON.stringify({
            error: "invalid_configuration",
            problems: [
              {
                pointer: "/mounts/home/mountPath",
                code: "overlapping_path",
                severity: "error",
              },
              { pointer: "/x", code: "Not A Code <script>", severity: "error" },
            ],
          }),
          { status: 422 },
        ),
      ),
    );
    const error = (await api.session().catch((e: unknown) => e)) as ApiError;
    expect(error.code).toBe("invalid_configuration");
    expect(error.problems).toHaveLength(1);
    expect(error.problems[0].message).toBe(
      "Two container mount paths overlap.",
    );
  });
  it("validates drafts with the CSRF token", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValue(
        new Response(JSON.stringify({ valid: true, problems: [] })),
      );
    vi.stubGlobal("fetch", fetch);
    await api.validate("templates", { schemaVersion: 2 }, "fixture-csrf");
    expect(fetch.mock.calls[0][0]).toBe("/api/v1/templates/validate");
    expect(fetch.mock.calls[0][1].headers["X-CSRF-Token"]).toBe("fixture-csrf");
  });
});
