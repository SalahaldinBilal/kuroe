import { describe, expect, it } from "vitest";
import type { Identity } from "./api";
import { expiryWarning, groupOf, listsRepos, pullsImages, scopeLine } from "./identities";

describe("listsRepos", () => {
  it("is every Git login with an API, not plain Git", () => {
    const kinds: Identity["kind"][] = ["github_app", "github_token", "gitlab", "gitea"];
    expect(kinds.every(listsRepos)).toBe(true);
    expect(listsRepos("git")).toBe(false);
    expect(listsRepos("registry")).toBe(false);
  });
});

const DAY = 86400;

describe("expiryWarning", () => {
  it("only warns inside two weeks", () => {
    const now = 1_000_000;
    expect(expiryWarning({ expires_at: null }, now)).toBeNull();
    expect(expiryWarning({ expires_at: now + 30 * DAY }, now)).toBeNull();
    expect(expiryWarning({ expires_at: now + 12 * DAY + 5 }, now)).toBe("Expires in 12 days");
    expect(expiryWarning({ expires_at: now + DAY + 5 }, now)).toBe("Expires in 1 day");
    expect(expiryWarning({ expires_at: now + 60 }, now)).toBe("Expires today");
    expect(expiryWarning({ expires_at: now - 60 }, now)).toBe("Expired");
  });
});

describe("groups and lines", () => {
  const base: Identity = {
    id: "1",
    kind: "registry",
    name: "hub",
    host: "docker.io",
    username: "acmebot",
    meta: {},
    created_at: 0,
    last_used_at: null,
    expires_at: null,
    used_by: [],
  };

  it("sorts kinds into the page's three cards", () => {
    expect(groupOf("github_app")).toBe("git");
    expect(groupOf("registry")).toBe("registry");
    expect(groupOf("cloudflare")).toBe("dns");
    expect(pullsImages("github_token")).toBe(true);
    expect(pullsImages("gitea")).toBe(false);
  });

  it("describes what a login reaches", () => {
    expect(scopeLine(base)).toBe("user acmebot on docker.io");
    expect(scopeLine({ ...base, kind: "github_app", meta: { owner: "acme", repositories: 14 } })).toBe(
      "14 repositories in acme",
    );
    expect(scopeLine({ ...base, kind: "cloudflare", meta: { zones: ["a.dev", "b.dev", "c.dev", "d.dev"] } })).toBe(
      "DNS on a.dev, b.dev, c.dev and 1 more",
    );
  });
});
