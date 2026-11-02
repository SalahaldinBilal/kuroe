import { describe, expect, it } from "vitest";
import { checkWindow, fieldText, isLive, monthCells, parseField, pickDay, rangeLabel, windowOf } from "./RangePicker";

const NOW = Date.UTC(2026, 10, 2, 12, 0, 0) / 1000;
const DAY = 86_400;

describe("time ranges", () => {
  it("presets follow now, custom windows stay put", () => {
    expect(windowOf({ preset: "6h" }, NOW)).toEqual({ from: NOW - 6 * 3_600, to: NOW });
    expect(windowOf({ from: 100, to: 5_000 }, NOW)).toEqual({ from: 100, to: 5_000 });
    expect(isLive({ preset: "1h" })).toBe(true);
    expect(isLive({ from: 1, to: 2 })).toBe(false);
  });

  it("reads and writes the fields in UTC", () => {
    expect(fieldText(NOW)).toBe("2026-11-02 12:00");
    expect(parseField("2026-11-02 12:00")).toBe(NOW);
    expect(parseField("2026-11-02T12:00:30")).toBe(NOW + 30);
    expect(parseField(" 2026-11-02 9:05 ")).toBe(NOW - 3 * 3_600 + 5 * 60);
    expect(parseField("2026-11-02 25:00")).toBeNull();
    expect(parseField("tomorrow")).toBeNull();
  });

  it("explains a window that doesn't work", () => {
    expect(checkWindow("2026-11-02 08:00", "2026-11-02 12:00")).toEqual({ from: NOW - 4 * 3_600, to: NOW });
    expect(checkWindow("2026-11-02 12:00", "2026-11-02 08:00")).toMatch("after the start");
    expect(checkWindow("2024-01-01 00:00", "2026-11-02 08:00")).toMatch("400 days");
    expect(checkWindow("soon", "2026-11-02 08:00")).toMatch("like 2026-11-02 08:00");
  });

  it("labels presets by name and custom windows by their times", () => {
    expect(rangeLabel({ preset: "24h" }, NOW)).toBe("Last 24 hours");
    expect(rangeLabel({ from: NOW - 4 * 3_600, to: NOW }, NOW)).toBe("Nov 2, 08:00 to 12:00 UTC");
    expect(rangeLabel({ from: NOW - DAY, to: NOW }, NOW)).toBe("Nov 1, 12:00 to Nov 2, 12:00 UTC");
    expect(rangeLabel({ from: NOW - 400 * DAY + 3 * DAY, to: NOW - 300 * DAY }, NOW)).toMatch(/2025/);
  });

  it("lays a month out in six weeks starting on Monday", () => {
    const cells = monthCells(2026, 10);
    expect(cells).toHaveLength(42);
    expect(new Date(cells[0] * 1000).getUTCDay()).toBe(1);
    expect(fieldText(cells[0]).slice(0, 10)).toBe("2026-10-26");
    expect(cells).toContain(Date.UTC(2026, 10, 30) / 1000);
  });

  it("a first click picks the start day, a second the end, keeping the times", () => {
    const nov1 = Date.UTC(2026, 10, 1) / 1000;
    const nov5 = Date.UTC(2026, 10, 5) / 1000;
    const oct28 = Date.UTC(2026, 9, 28) / 1000;
    const first = pickDay("2026-11-02 08:00", "2026-11-02 12:00", nov1, "start");
    expect(first).toEqual({ from: "2026-11-01 08:00", to: "2026-11-01 12:00", picking: "end" });
    const second = pickDay(first.from, first.to, nov5, first.picking);
    expect(second).toEqual({ from: "2026-11-01 08:00", to: "2026-11-05 12:00", picking: "start" });
    const backwards = pickDay(first.from, first.to, oct28, "end");
    expect(backwards).toEqual({ from: "2026-10-28 08:00", to: "2026-11-01 12:00", picking: "start" });
  });
});
