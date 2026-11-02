import { describe, expect, it } from "vitest";
import type { Domain } from "./api";
import { emptyPath, readErrorPage, retries, routingDraft, routingOf, routingProblem, routingSummary } from "./routing";

function domain(service: string, port: number, options: Domain["options"]): Domain {
  return {
    id: "dm1",
    deployment_id: "d1",
    hostname: "lufas.example.com",
    service,
    port,
    tls: true,
    probe_token: "",
    verified_at: null,
    created_at: 0,
    certificate: null,
    options,
    served_by: { kind: "acme", name: null, pinned: false },
  };
}

describe("domain routing drafts", () => {
  it("round trips paths, compression and error pages", () => {
    const options = {
      compress: true,
      error_page: { body: "<h1>Back soon</h1>" },
      paths: [
        { path: "/api/", service: "backend", port: 3420, error_page: { body: "{}", json: true } },
        { path: "/robots.txt", exact: true, service: "backend", port: 3420 },
      ],
    };
    const draft = routingDraft(domain("frontend", 3421, options), "app", "");
    expect(draft.kind).toBe("service");
    expect(draft.paths[1]).toEqual({ path: "/robots.txt", exact: true, service: "backend", port: "3420", page: null });
    expect(routingOf(draft)).toEqual({ service: "frontend", port: 3421, options });
  });

  it("round trips timeouts, body limits, retries, Host and compression settings", () => {
    const options = {
      compress: true,
      compress_types: ["text/html", "text/css"],
      compress_min_bytes: 1024,
      compress_gzip_only: true,
      retry: false,
      timeout: 60,
      max_body_mb: 1,
      rewrite_host: true,
    };
    const draft = routingDraft(domain("web", 5100, options), "app", "");
    expect(draft.compressTypes).toBe("text/html, text/css");
    expect(routingOf(draft)).toEqual({ service: "web", port: 5100, options });
    expect(routingOf({ ...draft, compress: false }).options.compress_types).toBeUndefined();
  });

  it("retries unless an error page applies, until set", () => {
    const base = routingDraft(null, "app", "80");
    expect(retries(base, false)).toBe(true);
    expect(retries(base, true)).toBe(false);
    expect(retries({ ...base, retry: true }, true)).toBe(true);
  });

  it("keeps a redirect on its own", () => {
    const draft = routingDraft(domain("", 0, { redirect: "https://lufas.example.com" }), "app", "80");
    expect(draft.kind).toBe("redirect");
    expect(draft.service).toBe("app");
    expect(draft.port).toBe("80");
    expect(routingOf({ ...draft, compress: true })).toEqual({
      service: "",
      port: 0,
      options: { redirect: "https://lufas.example.com" },
    });
  });

  it("starts empty and skips blank path rows", () => {
    const draft = { ...routingDraft(null, "app", ""), port: "80", paths: [emptyPath("app")] };
    expect(routingOf(draft)).toEqual({ service: "app", port: 80, options: {} });
  });

  it("says what is missing", () => {
    const base = routingDraft(null, "app", "");
    expect(routingProblem(base, false)).toBe("Pick a service and a port.");
    expect(routingProblem(base, true)).toBeNull();
    const withPath = { ...base, port: "80", paths: [{ ...emptyPath("api"), path: "/api/" }] };
    expect(routingProblem(withPath, false)).toBe("Pick a service and a port for /api/.");
    expect(routingProblem({ ...base, kind: "redirect" }, false)).toBe("Enter the address to redirect to.");
    const ready = { ...base, port: "80" };
    expect(routingProblem({ ...ready, timeout: "1.5" }, false)).toBe("The timeout is a whole number of seconds.");
    expect(routingProblem({ ...ready, maxBody: "0" }, false)).toBe("The body limit is a whole number of MB.");
    expect(routingProblem({ ...ready, timeout: "60", maxBody: "1" }, false)).toBeNull();
  });

  it("summarizes what a domain does", () => {
    expect(routingSummary({})).toBe("");
    expect(
      routingSummary({ compress: true, paths: [{ path: "/a/", service: "s", port: 1, error_page: { body: "x" } }] }),
    ).toBe("1 path, compressed, error page");
  });

  it("reads uploaded error pages as HTML or JSON", async () => {
    expect(await readErrorPage(new File(["<p>down</p>"], "maintenance.html", { type: "text/html" }))).toEqual({
      body: "<p>down</p>",
    });
    expect(await readErrorPage(new File(['{"error":"down"}'], "down.json"))).toEqual({
      body: '{"error":"down"}',
      json: true,
    });
    await expect(readErrorPage(new File(["  "], "empty.html"))).rejects.toThrow("empty.html is empty.");
  });
});
