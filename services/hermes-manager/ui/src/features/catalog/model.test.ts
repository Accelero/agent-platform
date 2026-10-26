import { describe, it, expect } from "vitest";
import { readFileSync, readdirSync } from "node:fs";
import {
  parseDraft,
  pointerRange,
  setPath,
  validateDraft,
  changedPaths,
} from "./model";
const fixtureRoot = new URL(
  "../../../../../../contracts/hermes-manager/schemas/fixtures/",
  import.meta.url,
);
describe("shared server/browser validation fixtures", () => {
  for (const [directory, valid] of [
    ["valid", true],
    ["invalid", false],
  ] as const) {
    for (const file of readdirSync(new URL(`${directory}/`, fixtureRoot))) {
      it(`${directory}/${file}`, () => {
        const value = JSON.parse(
          readFileSync(new URL(`${directory}/${file}`, fixtureRoot), "utf8"),
        );
        const collection = file.startsWith("terminal-backend")
          ? "terminal-backends"
          : "templates";
        expect(
          validateDraft(value, collection).filter(
            (problem) => problem.severity === "error",
          ).length === 0,
        ).toBe(valid);
      });
    }
  }
});
describe("canonical draft round trips", () => {
  for (const value of [
    undefined,
    null,
    "fixture-model",
    { provider: "example", advanced: [1, true, null] },
  ]) {
    it(`preserves unknown fields and explicit values for ${JSON.stringify(value)}`, () => {
      const original = {
        schemaVersion: 2,
        advanced: { nested: [{ unknown: true }] },
        adminConfig: {
          unknown: { nested: "kept" },
          ...(value === undefined ? {} : { model: value }),
        },
      };
      const text = JSON.stringify(original, null, 2);
      const parsed = parseDraft(text).value!;
      const changed = setPath(parsed, ["cpuMillis"], 2000);
      const returned = parseDraft(JSON.stringify(changed)).value!;
      expect(returned).toEqual({ ...original, cpuMillis: 2000 });
      expect(parsed).toEqual(original);
      const removed = setPath(
        returned,
        ["adminConfig", "model"],
        undefined,
        true,
      );
      expect(Object.hasOwn(removed.adminConfig as object, "model")).toBe(false);
    });
  }
  it("rejects duplicate keys and invalid raw JSON without a new canonical draft", () => {
    expect(parseDraft('{"model":1,"model":2}').error).toContain("Duplicate");
    expect(parseDraft('{"model":').value).toBeUndefined();
  });
  it("maps pointers to exact nested, escaped and array value ranges", () => {
    const text = '{"a":{"id":1},"list":[{"id":2}],"a/b":{"~":3}}';
    for (const [pointer, value] of [
      ["/a/id", "1"],
      ["/list/0/id", "2"],
      ["/a~1b/~0", "3"],
    ]) {
      const range = pointerRange(text, pointer);
      expect(text.slice(range.from, range.to)).toBe(value);
    }
  });
  it("reports changed semantic paths including removed managed overrides", () => {
    expect(
      changedPaths(
        { adminConfig: { model: "old", advanced: true } },
        { adminConfig: { advanced: true } },
      ),
    ).toEqual(["/adminConfig/model"]);
  });
});
