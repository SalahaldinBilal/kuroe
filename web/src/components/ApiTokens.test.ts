import { describe, expect, it } from "vitest";
import { claudeCommand, mcpUrl } from "./ApiTokens";

describe("MCP connection", () => {
  it("uses the public URL when there is one, else this page's address", () => {
    expect(mcpUrl("https://kuroe.example.com/")).toBe("https://kuroe.example.com/api/v1/mcp");
    expect(mcpUrl("  ")).toBe(`${location.origin}/api/v1/mcp`);
  });

  it("builds the Claude Code command with the token as a header", () => {
    expect(claudeCommand("https://k.example/api/v1/mcp", "kuroe_pat_x")).toBe(
      'claude mcp add --transport http kuroe https://k.example/api/v1/mcp --header "Authorization: Bearer kuroe_pat_x"',
    );
  });
});
