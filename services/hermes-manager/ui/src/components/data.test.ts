import { describe, it, expect } from "vitest";
import { cursorWindow, validatePage } from "./data";
describe("shareable bounded pagination", () => {
  it("continues past one hundred pages while keeping the current page and a bounded URL", () => {
    let cursors: string[] = [];
    let offset = 0;
    for (let page = 2; page <= 1000; page++) {
      const next = `opaque-${page}-${"x".repeat(170)}`;
      const window = cursorWindow(cursors, next);
      cursors = window.cursors;
      offset += window.removed;
      expect(cursors.at(-1)).toBe(next);
      expect(offset + cursors.length + 1).toBe(page);
      expect(
        encodeURIComponent(JSON.stringify(cursors)).length,
      ).toBeLessThanOrEqual(4096);
    }
  });
  it("accepts numeric and boolean external URL parameters without dropping filters", () => {
    const search = validatePage({
      assigned: true,
      runtimeVersion: 3,
      source: "manager",
      cursors: [],
    });
    expect(search.assigned).toBe("true");
    expect(search.runtimeVersion).toBe("3");
    expect(search.source).toBe("manager");
  });
  it("resets the page offset when filters return to the first page", () => {
    expect(validatePage({ cursors: [], pageOffset: 20 }).pageOffset).toBe(0);
  });
});
