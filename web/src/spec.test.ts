import { describe, expect, it } from "vitest";
import type { Source, Spec } from "./api";
import {
  envToRows,
  isCompose,
  isStatic,
  mbitText,
  mountsToRows,
  NODE_SITE,
  parseMbit,
  portsToRows,
  rowsToEnv,
  rowsToMounts,
  rowsToPorts,
  siteDraft,
  siteOf,
  sourceLabel,
  withEnvRefs,
} from "./spec";

describe("network limits", () => {
  it("round trips and refuses what isn't a positive number", () => {
    expect(parseMbit(" 5.5 ", "Upload")).toBe(5.5);
    expect(parseMbit("", "Upload")).toBeNull();
    expect(mbitText(null)).toBe("");
    expect(mbitText(20)).toBe("20");
    expect(() => parseMbit("0", "Upload")).toThrow("Upload must be a positive number of Mbit/s");
    expect(() => parseMbit("fast", "Upload")).toThrow();
  });
});

describe("sources", () => {
  const spec = (source: Source): Spec => ({ source, container: { ports: [], env: {} }, services: {} });

  it("treats a git repository with a Compose file as Compose", () => {
    const git = spec({ kind: "git", url: "https://g/r.git", reference: "main", compose_file: "compose.yaml" });
    expect(isCompose(git)).toBe(true);
    expect(sourceLabel(git)).toBe("https://g/r.git#main · compose.yaml");
    expect(isCompose(spec({ kind: "git", url: "https://g/r.git" }))).toBe(false);
    expect(isCompose(spec({ kind: "compose", file: "services: {}" }))).toBe(true);
  });

  it("knows static sites by their site options", () => {
    const archive = { ...spec({ kind: "static" }), site: siteOf(NODE_SITE) };
    expect(isStatic(archive)).toBe(true);
    expect(isStatic(spec({ kind: "static" }))).toBe(false);
    const repo = { ...spec({ kind: "git", url: "https://g/r.git" }), site: siteOf(NODE_SITE) };
    expect(isStatic(repo)).toBe(true);
    expect(sourceLabel(repo)).toBe("https://g/r.git · static site");
  });

  it("converts site settings both ways", () => {
    const node = siteOf(NODE_SITE);
    expect(node).toEqual({
      build: { kind: "command", image: "node:26-alpine", command: "npm ci && npm run build" },
      output: "dist",
      spa: true,
    });
    expect(siteDraft(node)).toEqual(NODE_SITE);
    expect(() => siteOf({ ...NODE_SITE, build: "dockerfile", output: "dist" })).toThrow();
    expect(siteOf({ ...NODE_SITE, build: "dockerfile", output: "/app/dist" }).build).toEqual({
      kind: "dockerfile",
      dockerfile: null,
    });
    expect(siteOf({ ...NODE_SITE, build: "none", output: "" })).toEqual({
      build: { kind: "none" },
      output: "",
      spa: true,
    });
  });
});

describe("ports", () => {
  it("round trips through rows", () => {
    const ports = [
      { host: 8080, container: 80, protocol: "tcp" as const },
      { host: null, container: 9000, protocol: "udp" as const },
    ];
    expect(rowsToPorts(portsToRows(ports))).toEqual(ports);
  });

  it("skips empty rows", () => {
    expect(rowsToPorts([{ host: "", container: "", protocol: "tcp" }])).toEqual([]);
  });

  it("rejects bad numbers", () => {
    expect(() => rowsToPorts([{ host: "8080", container: "abc", protocol: "tcp" }])).toThrow();
    expect(() => rowsToPorts([{ host: "70000", container: "80", protocol: "tcp" }])).toThrow();
    expect(() => rowsToPorts([{ host: "8080", container: "", protocol: "tcp" }])).toThrow();
  });
});

describe("env", () => {
  it("round trips plain values, raw secrets and saved secret references", () => {
    const rows = envToRows({ A: "1", TOKEN: "x", DB: "" }, ["TOKEN"], { DB: "s1" });
    expect(rows).toEqual([
      { key: "A", value: "1", secret: false, ref: null },
      { key: "TOKEN", value: "x", secret: true, ref: null },
      { key: "DB", value: "", secret: false, ref: "s1" },
    ]);
    expect(rowsToEnv(rows)).toEqual({ env: { A: "1", TOKEN: "x", DB: "" }, secrets: ["TOKEN"], refs: { DB: "s1" } });
  });

  it("never keeps a value behind a reference", () => {
    expect(rowsToEnv([{ key: "DB", value: "leftover", secret: true, ref: "s1" }])).toEqual({
      env: { DB: "" },
      secrets: [],
      refs: { DB: "s1" },
    });
  });

  it("keeps the git token reference when env references change", () => {
    const spec = { secret_refs: { "@git_token": "t1", OLD: "s0" } } as unknown as Spec;
    expect(withEnvRefs(spec, { DB: "s1" })).toEqual({ DB: "s1", "@git_token": "t1" });
  });

  it("skips empty rows and trims keys", () => {
    expect(
      rowsToEnv([
        { key: " A ", value: "1", secret: false, ref: null },
        { key: "", value: "", secret: true, ref: null },
      ]),
    ).toEqual({
      env: { A: "1" },
      secrets: [],
      refs: {},
    });
  });

  it("rejects values without a key and duplicate keys", () => {
    expect(() => rowsToEnv([{ key: "", value: "x", secret: false, ref: null }])).toThrow();
    expect(() => rowsToEnv([{ key: "", value: "", secret: false, ref: "s1" }])).toThrow();
    expect(() =>
      rowsToEnv([
        { key: "A", value: "1", secret: false, ref: null },
        { key: "A", value: "2", secret: false, ref: null },
      ]),
    ).toThrow();
  });
});

describe("mounts", () => {
  it("round trips", () => {
    const mounts = [{ volume: "uploads", target: "/srv", read_only: true }];
    expect(rowsToMounts(mountsToRows(mounts))).toEqual(mounts);
  });

  it("needs an absolute path and a volume", () => {
    expect(() => rowsToMounts([{ volume: "uploads", target: "srv", readOnly: false }])).toThrow();
    expect(() => rowsToMounts([{ volume: "", target: "/srv", readOnly: false }])).toThrow();
    expect(rowsToMounts([{ volume: "uploads", target: " ", readOnly: false }])).toEqual([]);
  });
});
