import { describe, expect, it } from "vitest";
import { numberedLines } from "./FilePreview";
import { join, parent } from "./Files";

describe("file paths", () => {
  it("joins and goes up", () => {
    expect(join("/", "etc")).toBe("/etc");
    expect(join("/etc", "ssl")).toBe("/etc/ssl");
    expect(parent("/etc/ssl/")).toBe("/etc");
    expect(parent("/etc")).toBe("/");
  });
});

describe("numberedLines", () => {
  it("drops the empty line after a final newline only", () => {
    expect(numberedLines("a\nb\n")).toEqual(["a", "b"]);
    expect(numberedLines("a\n\n")).toEqual(["a", ""]);
    expect(numberedLines("")).toEqual([""]);
  });
});
