import { describe, expect, it } from "vitest";
import { renderMarkdown } from "./Markdown";

describe("release notes markdown", () => {
  it("renders the changelog's headings, lists and inline code", () => {
    const html = renderMarkdown(
      '## 0.2.2 - 2026-11-06\n\n- Updating no longer rolls back on servers with HTTP access set to "local".\n  The check now uses `/api/v1/health`.',
    );
    expect(html).toContain("<h2>0.2.2 - 2026-11-06</h2>");
    expect(html).toContain("<li>");
    expect(html).toContain("<code>/api/v1/health</code>");
  });

  it("strips scripts and event handlers from a tampered changelog", () => {
    const html = renderMarkdown('- fixes <img src=x onerror="alert(1)"><script>alert(2)</script>');
    expect(html).not.toContain("onerror");
    expect(html).not.toContain("<script");
  });
});
