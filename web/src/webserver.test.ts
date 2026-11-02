import { describe, expect, it } from "vitest";
import { extraFile, serverDefaults, serverDraft, serverOf, switchKind } from "./webserver";

describe("web server drafts", () => {
  it("round trips through the API shape", () => {
    const draft = { ...serverDefaults("nginx", "web"), mode: "passthrough" as const };
    const api = serverOf(draft);
    expect(api.container).toBe("web");
    expect(api.extra_path).toBe("/etc/nginx/kuroe-stream.conf");
    expect(serverDraft(api)).toEqual(draft);
  });

  it("drops the container when it runs on this machine", () => {
    const draft = { ...serverDefaults("nginx", "web"), where: "host" as const };
    expect(serverOf(draft).container).toBeNull();
  });

  it("falls back to front for servers without passthrough", () => {
    const passthrough = { ...serverDefaults("nginx"), mode: "passthrough" as const };
    const apache = switchKind(passthrough, "apache");
    expect(apache.mode).toBe("front");
    expect(apache.path).toBe("/etc/apache2/sites-enabled/kuroe.conf");
    expect(switchKind(passthrough, "haproxy").mode).toBe("passthrough");
    expect(serverOf({ ...serverDefaults("caddy"), mode: "passthrough" }).mode).toBe("front");
  });

  it("asks for a second file only where one is written", () => {
    expect(extraFile("nginx", "front")).toBeNull();
    expect(extraFile("nginx", "passthrough")).toBe("Stream config file");
    expect(extraFile("haproxy", "front")).toBe("Hostname map file");
    expect(extraFile("caddy", "front")).toBeNull();
  });
});
