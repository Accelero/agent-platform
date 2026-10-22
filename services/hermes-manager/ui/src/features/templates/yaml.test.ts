import { expect, it } from "vitest";
import {
  parseConfiguration,
  serializeConfiguration,
  configurationRange,
} from "./yaml";
it("round trips JSON and YAML without losing unknown fields, null or scalar types", () => {
  const value = {
    schemaVersion: 1,
    adminConfig: {
      advanced: [true, null, "yes", "2026-10-22", { nested: 42 }],
    },
  };
  const text = serializeConfiguration(value, "yaml");
  expect(parseConfiguration(text, "yaml").value).toEqual(value);
  expect(
    parseConfiguration(serializeConfiguration(value, "json")).value,
  ).toEqual(value);
});
for (const text of [
  "a: 1\na: 2",
  "a: &x [1]\nb: *x",
  "a: !!timestamp 2026-10-22",
  "? [a, b]\n: value",
  "a: .nan",
  "a: 9007199254740993",
  "a: 1\n---\nb: 2",
  "a: {<<: {b: 1}}",
])
  it(`rejects non-JSON YAML: ${text}`, () =>
    expect(parseConfiguration(text, "yaml").error).toBeTruthy());
it("addresses YAML values with escaped JSON pointers", () => {
  const text = "adminConfig:\n  a/b: false\n";
  const range = configurationRange(text, "/adminConfig/a~1b", "yaml");
  expect(text.slice(range.from, range.to)).toBe("false");
});
