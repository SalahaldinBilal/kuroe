import { describe, expect, it } from "vitest";
import { formatAgo, formatDuration } from "./format";

describe("formatDuration", () => {
  it("keeps tenths only for short steps", () => {
    expect(formatDuration(0.42)).toBe("0.4s");
    expect(formatDuration(42.7)).toBe("42s");
    expect(formatDuration(192)).toBe("3m 12s");
    expect(formatDuration(3900)).toBe("1h 5m");
    expect(formatDuration(-1)).toBe("0.0s");
  });
});

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
