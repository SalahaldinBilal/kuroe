import { describe, expect, it } from "vitest";
import { scopeLabel, togglePermission } from "./permissions";

describe("togglePermission", () => {
  it("keeps the list in the display order and drops duplicates", () => {
    expect(togglePermission(["users", "dep.view"], "dep.deploy", true)).toEqual(["dep.view", "dep.deploy", "users"]);
    expect(togglePermission(["dep.view"], "dep.view", true)).toEqual(["dep.view"]);
    expect(togglePermission(["dep.view", "dep.deploy"], "dep.view", false)).toEqual(["dep.deploy"]);
  });
});

describe("scopeLabel", () => {
  it("names deployments and marks ones that are gone", () => {
    const names = new Map([["d1", "shop"]]);
    expect(scopeLabel("all", names)).toBe("All deployments");
    expect(scopeLabel(["d1", "d9"], names)).toBe("shop, deleted");
    expect(scopeLabel([], names)).toBe("No deployments");
  });
});
