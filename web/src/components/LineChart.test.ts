import { describe, expect, it } from "vitest";
import { gapFor, OTHER, runs, topSeries, yAxis } from "./LineChart";

const MIB = 2 ** 20;
const GIB = 2 ** 30;

describe("yAxis", () => {
  it("puts the top close above the peak", () => {
    for (const peak of [GIB, 2 * GIB, 12.5e6, 400, 37, 1.01e9, 700 * MIB, 3]) {
      const { top } = yAxis(peak, peak > 1000);
      expect(top).toBeGreaterThanOrEqual(peak);
      expect(top / peak).toBeLessThanOrEqual(1.15);
    }
  });

  it("lands binary limits on a gridline", () => {
    expect(yAxis(512 * MIB, true).ticks).toEqual([0, 128, 256, 384, 512].map((v) => v * MIB));
    expect(yAxis(GIB, true).top).toBe(GIB);
    expect(yAxis(2 * GIB, true).top).toBe(2 * GIB);
  });

  it("uses round decimal steps", () => {
    expect(yAxis(400).ticks).toEqual([0, 100, 200, 300, 400]);
    expect(yAxis(100).top).toBe(100);
  });

  it("still draws an axis when everything is zero", () => {
    expect(yAxis(0).top).toBe(1);
    expect(yAxis(0, true).top).toBe(1024);
  });
});

describe("topSeries", () => {
  const s = (label: string, v: number) => ({
    label,
    points: [
      { t: 0, v },
      { t: 10, v: v / 2 },
    ],
  });

  it("keeps eight or fewer series as they are", () => {
    const few = [s("a", 1), s("b", 2)];
    expect(topSeries(few)).toBe(few);
  });

  it("keeps the busiest seven and adds up the rest", () => {
    const many = Array.from({ length: 10 }, (_, i) => s(`d${i}`, i + 1));
    const out = topSeries(many);
    expect(out.map((x) => x.label)).toEqual(["d9", "d8", "d7", "d6", "d5", "d4", "d3", OTHER]);
    expect(out[7].points).toEqual([
      { t: 0, v: 6 },
      { t: 10, v: 3 },
    ]);
  });
});

describe("runs", () => {
  it("keeps evenly spaced samples together", () => {
    const points = [0, 30, 60, 90].map((t) => ({ t, v: 1 }));
    expect(runs(points, gapFor(15, 30))).toHaveLength(1);
  });

  it("breaks where samples are missing", () => {
    const points = [0, 30, 60, 5000, 5030].map((t) => ({ t, v: 1 }));
    expect(runs(points, gapFor(15, 30)).map((r) => r.length)).toEqual([3, 2]);
  });

  it("breaks on null values and keeps a lone sample as its own run", () => {
    const points = [
      { t: 0, v: 1 },
      { t: 30, v: null },
      { t: 60, v: 2 },
      { t: 9000, v: 3 },
    ];
    expect(runs(points, gapFor(15, 30)).map((r) => r.length)).toEqual([1, 1, 1]);
  });
});
