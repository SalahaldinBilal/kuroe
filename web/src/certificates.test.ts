import { describe, expect, it } from "vitest";
import type { Identity } from "./api";
import { certificatesFor, covers, servedLabel, zoneHostnames } from "./certificates";

const cert = (id: string, sans: string[]): Identity => ({
  id,
  kind: "certificate",
  name: id,
  host: "",
  username: "",
  meta: { sans },
  created_at: 0,
  last_used_at: null,
  expires_at: null,
  used_by: [],
});

describe("covers", () => {
  it("matches the server's wildcard rule", () => {
    expect(covers("*.example.com", "app.example.com")).toBe(true);
    expect(covers("*.example.com", "example.com")).toBe(false);
    expect(covers("*.example.com", "a.b.example.com")).toBe(false);
    expect(covers("example.com", "EXAMPLE.com")).toBe(true);
  });
});

describe("picking certificates", () => {
  it("offers only certificates that cover the hostname", () => {
    const all = [cert("origin", ["example.com", "*.example.com"]), cert("other", ["other.dev"])];
    expect(certificatesFor(all, "shop.example.com").map((c) => c.id)).toEqual(["origin"]);
    expect(certificatesFor(all, "deep.shop.example.com")).toEqual([]);
  });

  it("labels what a domain gets", () => {
    expect(servedLabel({ kind: "saved", name: "origin", pinned: false })).toBe("origin, automatic");
    expect(servedLabel({ kind: "acme", name: null, pinned: true })).toBe("Let's Encrypt, pinned");
    expect(servedLabel({ kind: "default", name: null, pinned: false })).toBe("Traefik default");
    expect(zoneHostnames("example.com")).toEqual(["example.com", "*.example.com"]);
  });
});
