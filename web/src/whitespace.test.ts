import { describe, expect, it } from "vitest";
import { spaceRuns, whitespaceProblem } from "./whitespace";

describe("whitespace in env and secret values", () => {
  it("catches a space copied along with a connection string", () => {
    expect(whitespaceProblem("postgres://app:pw@db:5432/app ")).toBe("Ends with a space");
  });

  it("catches a token pasted from a web page with a non-breaking space", () => {
    expect(whitespaceProblem(" ghp_8f2Kd9sLq0")).toBe("Starts with a non-breaking space");
  });

  it("catches a value copied from a terminal with a tab on both sides", () => {
    expect(whitespaceProblem("\tsk_live_51H\t")).toBe("Starts with a tab and ends with a tab");
  });

  it("leaves normal values with spaces inside alone", () => {
    expect(whitespaceProblem("Acme Corp <noreply@acme.dev>")).toBeNull();
    expect(whitespaceProblem("-Xmx512m -Xms256m")).toBeNull();
    expect(whitespaceProblem("")).toBeNull();
  });

  it("marks only the spaces at the ends as likely mistakes", () => {
    const runs = spaceRuns("  -Xmx512m -Xms256m ");
    expect(runs.filter((r) => r.space).map((r) => [r.text, r.edge])).toEqual([
      ["  ", true],
      [" ", false],
      [" ", true],
    ]);
    expect(runs.map((r) => r.text).join("")).toBe("  -Xmx512m -Xms256m ");
  });
});
