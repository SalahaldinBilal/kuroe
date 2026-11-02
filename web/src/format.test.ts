import { describe, expect, it } from "vitest";
import { formatAgo } from "./format";

describe("formatAgo", () => {
  const now = Date.UTC(2026, 9, 31, 12, 0) / 1000;

  it("reads like a person would say it", () => {
    expect(formatAgo(now - 10, now)).toBe("just now");
    expect(formatAgo(now - 12 * 60, now)).toBe("12 min ago");
    expect(formatAgo(now - 3600, now)).toBe("1 hour ago");
    expect(formatAgo(now - 5 * 3600, now)).toBe("5 hours ago");
    expect(formatAgo(now - 30 * 3600, now)).toBe("yesterday");
    expect(formatAgo(now - 3 * 86_400, now)).toBe("3 days ago");
    expect(formatAgo(now - 20 * 86_400, now)).toBe("2026-10-11");
  });

  it("treats future times as now", () => {
    expect(formatAgo(now + 60, now)).toBe("just now");
  });
});
