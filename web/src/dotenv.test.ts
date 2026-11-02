import { describe, expect, it } from "vitest";
import { mergeEnv, mergeSummary, parseDotenv } from "./dotenv";

describe("parseDotenv", () => {
  it("skips blank lines and comments and strips export", () => {
    const text = "\n# a comment\n  \nexport A=1\nB = two\n";
    expect(parseDotenv(text)).toEqual({
      entries: [
        { key: "A", value: "1" },
        { key: "B", value: "two" },
      ],
      invalid: 0,
    });
  });

  it("splits on the first equals sign", () => {
    expect(parseDotenv("URL=postgres://u:p@db/app?x=1").entries).toEqual([
      { key: "URL", value: "postgres://u:p@db/app?x=1" },
    ]);
  });

  it("strips matching quotes and unescapes newlines in double quotes only", () => {
    const text = `A="line one\\nline two"\nB='raw\\n'\nC="say \\"hi\\""`;
    expect(parseDotenv(text).entries).toEqual([
      { key: "A", value: "line one\nline two" },
      { key: "B", value: "raw\\n" },
      { key: "C", value: 'say "hi"' },
    ]);
  });

  it("strips trailing comments from unquoted values but not from quoted ones", () => {
    const text = 'A=value # note\nB="keep # this"\nC=no#comment';
    expect(parseDotenv(text).entries).toEqual([
      { key: "A", value: "value" },
      { key: "B", value: "keep # this" },
      { key: "C", value: "no#comment" },
    ]);
  });

  it("reads quoted values over several lines", () => {
    const text = 'KEY="-----BEGIN-----\nabc\n-----END-----"\nNEXT=1';
    expect(parseDotenv(text).entries).toEqual([
      { key: "KEY", value: "-----BEGIN-----\nabc\n-----END-----" },
      { key: "NEXT", value: "1" },
    ]);
  });

  it("counts lines with bad keys or no equals sign as invalid", () => {
    expect(parseDotenv("1BAD=x\nno equals\nGOOD.key=y\n-X=1")).toEqual({
      entries: [{ key: "GOOD.key", value: "y" }],
      invalid: 3,
    });
  });

  it("keeps the last value of a duplicate key", () => {
    expect(parseDotenv("A=1\nA=2").entries).toEqual([{ key: "A", value: "2" }]);
  });

  it("handles windows line endings", () => {
    expect(parseDotenv("A=1\r\nB=2\r\n").entries).toEqual([
      { key: "A", value: "1" },
      { key: "B", value: "2" },
    ]);
  });
});

describe("mergeEnv", () => {
  it("updates existing keys, adds new ones, drops empty rows and locks secret looking keys", () => {
    const rows = [
      { key: "A", value: "old", secret: false, ref: null },
      { key: "", value: "", secret: false, ref: null },
    ];
    const merged = mergeEnv(rows, [
      { key: "A", value: "new" },
      { key: "DB_PASSWORD", value: "x" },
      { key: "API_KEY", value: "y" },
      { key: "PORT", value: "80" },
    ]);
    expect(merged).toEqual({
      rows: [
        { key: "A", value: "new", secret: false, ref: null },
        { key: "DB_PASSWORD", value: "x", secret: true, ref: null },
        { key: "API_KEY", value: "y", secret: true, ref: null },
        { key: "PORT", value: "80", secret: false, ref: null },
      ],
      added: 3,
      updated: 1,
    });
    expect(mergeSummary(merged)).toBe("Added 3 variables, updated 1");
  });

  it("does not unlock an existing secret", () => {
    const merged = mergeEnv([{ key: "A", value: "1", secret: true, ref: null }], [{ key: "A", value: "2" }]);
    expect(merged.rows).toEqual([{ key: "A", value: "2", secret: true, ref: null }]);
    expect(mergeSummary(merged)).toBe("Updated 1 variable");
  });

  it("replaces a saved secret reference with the pasted raw value, kept secret", () => {
    const merged = mergeEnv([{ key: "DB", value: "", secret: false, ref: "s1" }], [{ key: "DB", value: "raw" }]);
    expect(merged.rows).toEqual([{ key: "DB", value: "raw", secret: true, ref: null }]);
  });
});
