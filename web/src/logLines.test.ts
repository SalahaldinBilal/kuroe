import { describe, expect, it } from "vitest";
import { clockTime, parseLogLine, splitLines } from "./logLines";

const E = "\u001b";

describe("saved log lines", () => {
  it("shows the message without kuroe's prefix, keeping its colors", () => {
    const entry = parseLogLine(
      `2026-11-04T10:02:52.433147784Z stderr 1ed0a3280728 ${E}[32mINFO${E}[0m listening on 0.0.0.0:8080`,
    );
    expect(clockTime(entry.stamp ?? "")).toBe("10:02:52");
    expect(entry.container).toBe("1ed0a3280728");
    expect(entry.segments.map((s) => s.text).join("")).toBe("INFO listening on 0.0.0.0:8080");
    expect(entry.segments[0].style.fg).toBe("var(--ansi-2)");
  });

  it("keeps a blank line the container printed", () => {
    const entry = parseLogLine("2026-11-04T10:02:54.449255594Z stdout 1ed0a3280728 ");
    expect(entry.stamp).toBe("2026-11-04T10:02:54.449255594Z");
    expect(entry.segments).toEqual([]);
  });

  it("puts a line cut between two socket messages back together", () => {
    const first = splitLines("2026-11-04T10:02:52Z stdout 1ed0a3280728 GET /health 200\n2026-11-04T10:02:53Z std");
    expect(first.lines).toHaveLength(1);
    const second = splitLines(`${first.rest}out 1ed0a3280728 GET / 200\n`);
    expect(second.lines.map((l) => parseLogLine(l).search)).toEqual(["get / 200"]);
    expect(second.rest).toBe("");
  });
});
