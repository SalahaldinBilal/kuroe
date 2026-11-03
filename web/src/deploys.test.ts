import { describe, expect, it } from "vitest";
import type { DeploySummary } from "./api";
import { commitUrl, sourceText, stepSeconds, stepText, triggerText } from "./deploys";

const SHA = "3f2a9c1d8e7b6a5f4e3d2c1b0a9f8e7d6c5b4a39";

describe("commitUrl", () => {
  it("knows each provider's commit page", () => {
    expect(commitUrl({ kind: "git", url: "https://github.com/acme/api.git", provider: "github", commit: SHA })).toBe(
      `https://github.com/acme/api/commit/${SHA}`,
    );
    expect(
      commitUrl({ kind: "git", url: "https://gitlab.example.com/group/sub/api", provider: "gitlab", commit: SHA }),
    ).toBe(`https://gitlab.example.com/group/sub/api/-/commit/${SHA}`);
    expect(
      commitUrl({ kind: "git", url: "https://git.example.com/acme/api.git/", provider: "gitea", commit: SHA }),
    ).toBe(`https://git.example.com/acme/api/commit/${SHA}`);
  });

  it("gives nothing without a known provider, a commit or an https URL", () => {
    expect(commitUrl({ kind: "git", url: "https://bitbucket.org/acme/api.git", commit: SHA })).toBeNull();
    expect(commitUrl({ kind: "git", url: "https://github.com/acme/api.git", provider: "github" })).toBeNull();
    expect(commitUrl({ kind: "git", url: "git@github.com:acme/api.git", provider: "github", commit: SHA })).toBeNull();
  });
});

describe("sourceText", () => {
  it("shows the short commit, else the image and digest", () => {
    expect(sourceText({ kind: "git", reference: "main", commit: SHA })).toBe("3f2a9c1");
    expect(sourceText({ kind: "git", reference: "main" })).toBe("main");
    expect(sourceText({ kind: "image", image: "nginx:1.29", digest: `sha256:${"ab".repeat(32)}` })).toBe(
      "nginx:1.29@abababababab",
    );
    expect(sourceText({ kind: "compose" })).toBe("compose");
  });
});

describe("triggerText", () => {
  const deploy = (trigger: DeploySummary["trigger"], by: string): DeploySummary => ({
    id: "d",
    revision: 1,
    status: "running",
    trigger,
    triggered_by: by,
    user_id: null,
    token_id: null,
    source: { kind: "git" },
    started_at: 0,
    finished_at: 1,
  });

  it("says who or what started it", () => {
    expect(triggerText(deploy("ui", "boss"))).toBe("boss");
    expect(triggerText(deploy("deploy_token", "ci"))).toBe("token ci");
    expect(triggerText(deploy("webhook", "GitHub push by octo"))).toBe("GitHub push by octo");
  });
});

describe("steps", () => {
  it("counts a running step up to now and a waiting one as nothing", () => {
    expect(stepSeconds({ step: "cloning", started_ms: 1000, finished_ms: 3500 }, 9000)).toBe(2.5);
    expect(stepSeconds({ step: "building", started_ms: 1000, finished_ms: null }, 9000)).toBe(8);
    expect(stepSeconds({ step: "starting", started_ms: null, finished_ms: null }, 9000)).toBe(0);
  });

  it("names the step with what it works on", () => {
    expect(stepText("building", "web")).toBe("Building web");
    expect(stepText("pulling", null)).toBe("Pulling");
  });
});
