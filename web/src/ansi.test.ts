import { describe, expect, it } from "vitest";
import { applySgr, color256, parseAnsi, parseLine, stripAnsi } from "./ansi";

const E = "\u001b";

describe("ansi", () => {
  it("turns color codes into styled segments", () => {
    const { segments, end } = parseLine(`${E}[91m  Downloaded itoa v1.0.18${E}[0m done`, {});
    expect(segments).toEqual([
      { text: "  Downloaded itoa v1.0.18", style: { fg: "var(--ansi-9)" } },
      { text: " done", style: {} },
    ]);
    expect(end).toEqual({});
  });

  it("reads bold, 256 and true colors, and resets parts", () => {
    expect(applySgr({}, "1;31")).toEqual({ bold: true, fg: "var(--ansi-1)" });
    expect(applySgr({}, "38;5;196").fg).toBe("rgb(255 0 0)");
    expect(applySgr({}, "48;5;244").bg).toBe("rgb(128 128 128)");
    expect(applySgr({}, "38;2;10;20;30").fg).toBe("rgb(10 20 30)");
    expect(applySgr({ bold: true, fg: "x" }, "22;39")).toEqual({ bold: false, dim: false, fg: undefined });
    expect(applySgr({ fg: "x" }, "")).toEqual({});
    expect(color256(300)).toBeUndefined();
  });

  it("drops other escapes and keeps the last frame of a redrawn line", () => {
    expect(stripAnsi(`${E}]0;title${E}\\${E}[2Kafter${E}[1G`)).toBe("after");
    expect(stripAnsi(`${E}]8;;https://x${E}\\link${E}]8;;${E}\\`)).toBe("link");
    expect(parseLine("fetch 10%\rfetch 100%\r", {}).segments).toEqual([{ text: "fetch 100%", style: {} }]);
  });

  it("carries a color to the next line and reuses unchanged lines", () => {
    const cache = new Map();
    const first = parseAnsi(`${E}[32mgreen\nstill green${E}[0m\nplain`, cache);
    expect(first[1]).toEqual([{ text: "still green", style: { fg: "var(--ansi-2)" } }]);
    expect(first[2]).toEqual([{ text: "plain", style: {} }]);
    const second = parseAnsi(`${E}[32mgreen\nstill green${E}[0m\nplain\nnew`, cache);
    expect(second[0]).toBe(first[0]);
    expect(second[2]).toBe(first[2]);
    expect(cache.size).toBe(4);
  });
});
