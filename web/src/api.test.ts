import { describe, expect, it } from "vitest";
import { failureText } from "./api";

describe("a failed request's message", () => {
  it("uses kuroe's own error", () => {
    const body = JSON.stringify({ error: "the provider refused the webhook (404 Not Found): Not Found" });
    expect(failureText(502, body)).toBe("the provider refused the webhook (404 Not Found): Not Found");
  });

  it("shows kuroe's plain text HTTPS refusal", () => {
    const body =
      "kuroe only answers over HTTPS here. Use https://kuroe.example.com, or run `kuroe admin http open` on the server.\n";
    expect(failureText(403, body)).toBe(body.trim());
  });

  it("shows what Traefik answers when kuroe can't be reached", () => {
    expect(failureText(502, "Bad Gateway")).toBe("Bad Gateway");
  });

  it("names the status when a proxy answers with an HTML page or nothing at all", () => {
    const nginx =
      "<html>\r\n<head><title>504 Gateway Time-out</title></head>\r\n<body>\r\n<center><h1>504 Gateway Time-out</h1></center>\r\n<hr><center>nginx</center>\r\n</body>\r\n</html>\r\n";
    expect(failureText(504, nginx)).toBe("The request failed with status 504 Gateway Timeout.");
    expect(failureText(502, "")).toBe("The request failed with status 502 Bad Gateway.");
  });
});
