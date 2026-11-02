import { describe, expect, it } from "vitest";
import { gapFor, runs } from "./LineChart";

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
