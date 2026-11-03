import { describe, expect, it } from "vitest";
import { highlight } from "./highlight";
import {
  addToHistory,
  bodyKind,
  encodeBody,
  folderOf,
  fromBase64,
  hexDump,
  languageOf,
  outgoingHeaders,
  type SavedRequest,
  statusTone,
  toBase64,
  withBase,
  withQuery,
} from "./preview";

const bytes = (text: string) => new TextEncoder().encode(text);
const decoded = (b64: string | null) => new TextDecoder().decode(fromBase64(b64 ?? ""));

describe("requests", () => {
  it("adds params to the path's own query", () => {
    expect(withQuery("/items", [{ key: "page", value: "2" }])).toBe("/items?page=2");
    expect(
      withQuery("/items?sort=new", [
        { key: "q", value: "a b" },
        { key: " ", value: "x" },
      ]),
    ).toBe("/items?sort=new&q=a+b");
    expect(withQuery("/", [])).toBe("/");
  });

  it("encodes each kind of body with its type", () => {
    expect(encodeBody("none", "ignored", "", [])).toEqual({ body: null, contentType: null });
    const json = encodeBody("json", '{"a":1}', "", []);
    expect([decoded(json.body), json.contentType]).toEqual(['{"a":1}', "application/json"]);
    expect(encodeBody("json", "{nope", "", []).error).toContain("isn't valid");
    const form = encodeBody("form", "", "", [
      { key: "name", value: "kuro e" },
      { key: "", value: "x" },
    ]);
    expect([decoded(form.body), form.contentType]).toEqual(["name=kuro+e", "application/x-www-form-urlencoded"]);
    expect(encodeBody("raw", "<a/>", "application/xml", []).contentType).toBe("application/xml");
    expect(encodeBody("raw", "hi", " ", []).contentType).toBe("text/plain");
  });

  it("adds the body's type unless a header already sets one", () => {
    expect(
      outgoingHeaders(
        [
          { key: "X-A", value: "1" },
          { key: "", value: "2" },
        ],
        "application/json",
      ),
    ).toEqual([
      ["X-A", "1"],
      ["Content-Type", "application/json"],
    ]);
    expect(outgoingHeaders([{ key: "content-type", value: "text/csv" }], "text/plain")).toEqual([
      ["content-type", "text/csv"],
    ]);
  });

  it("round-trips bytes through base64, past the chunk size", () => {
    const big = new Uint8Array(70_000).map((_, i) => i % 256);
    expect(fromBase64(toBase64(big))).toEqual(big);
  });
});

describe("answers", () => {
  it("picks a viewer from the content type", () => {
    expect(bodyKind("text/html; charset=utf-8", bytes("<p>"))).toBe("html");
    expect(bodyKind("application/problem+json", bytes("{}"))).toBe("json");
    expect(bodyKind("image/svg+xml", bytes("<svg/>"))).toBe("image");
    expect(bodyKind("application/pdf", bytes("%PDF"))).toBe("pdf");
    expect(bodyKind("application/javascript", bytes("x"))).toBe("text");
    expect(bodyKind("video/mp4", bytes("x"))).toBe("video");
  });

  it("looks at the bytes when the type says nothing", () => {
    expect(bodyKind(undefined, bytes("plain words"))).toBe("text");
    expect(bodyKind("application/octet-stream", new Uint8Array([0x89, 0x50, 0x00, 0x01]))).toBe("binary");
    const cutInHalf = bytes(`${"a".repeat(1023)}é`);
    expect(bodyKind(undefined, cutInHalf)).toBe("text");
    expect(bodyKind(undefined, new Uint8Array())).toBe("text");
  });

  it("highlights by language", () => {
    expect(languageOf("text/html")).toBe("xml");
    expect(languageOf("text/css")).toBe("css");
    expect(languageOf("text/javascript")).toBe("js");
    expect(languageOf("text/plain")).toBe("plain");
  });

  it("shows binary as hex with printable text", () => {
    const lines = hexDump(bytes("Hello, kuroe!\n\u0001xyz"), 4096);
    expect(lines[0]).toBe("00000000  48 65 6c 6c 6f 2c 20 6b 75 72 6f 65 21 0a 01 78  |Hello, kuroe!..x|");
    expect(lines[1].startsWith("00000010  79 7a")).toBe(true);
    expect(hexDump(new Uint8Array(100), 32)).toHaveLength(2);
  });

  it("points a rendered page's links at the preview proxy", () => {
    expect(withBase("<html><head><title>x</title></head></html>", "/preview/c1/80/")).toBe(
      '<html><head><base href="/preview/c1/80/"><title>x</title></head></html>',
    );
    expect(withBase("<p>bare</p>", "/p/")).toBe('<base href="/p/"><p>bare</p>');
    expect(folderOf("/shop/cart?x=1")).toBe("/shop/");
    expect(folderOf("/")).toBe("/");
  });

  it("colors statuses", () => {
    expect([statusTone(200), statusTone(301), statusTone(404), statusTone(502)]).toEqual([
      "ok",
      undefined,
      "warn",
      "bad",
    ]);
  });
});

describe("history", () => {
  const entry = (path: string, at: number, status?: number): SavedRequest => ({
    container: "c1",
    port: 80,
    method: "GET",
    path,
    params: [],
    headers: [],
    mode: "none",
    text: "",
    rawType: "",
    form: [],
    status,
    at,
  });

  it("keeps the newest first and drops a repeat", () => {
    let list: SavedRequest[] = [];
    list = addToHistory(list, entry("/a", 1, 200));
    list = addToHistory(list, entry("/b", 2, 200));
    list = addToHistory(list, entry("/a", 3, 500));
    expect(list.map((e) => [e.path, e.status])).toEqual([
      ["/a", 500],
      ["/b", 200],
    ]);
    for (let i = 0; i < 30; i++) list = addToHistory(list, entry(`/${i}`, i));
    expect(list).toHaveLength(20);
  });
});

describe("highlight", () => {
  const kinds = (text: string, language: Parameters<typeof highlight>[1]) =>
    highlight(text, language).map((line) => line.map((t) => `${t.kind}:${t.text}`));

  it("colors json keys apart from values", () => {
    expect(kinds('{"a": "b", "n": 1.5, "t": true}', "json")).toEqual([
      [
        "plain:{",
        'attr:"a"',
        "plain:: ",
        'string:"b"',
        "plain:, ",
        'attr:"n"',
        "plain:: ",
        "number:1.5",
        "plain:, ",
        'attr:"t"',
        "plain:: ",
        "keyword:true",
        "plain:}",
      ],
    ]);
  });

  it("keeps a block comment's color across lines", () => {
    expect(kinds("/* one\ntwo */ return x", "js")).toEqual([
      ["comment:/* one"],
      ["comment:two */", "plain: ", "keyword:return", "plain: x"],
    ]);
  });

  it("colors tags, attributes and their values", () => {
    expect(kinds('<a href="/x">hi</a>', "xml")).toEqual([
      ["tag:<a", "plain: ", "attr:href", "plain:=", 'string:"/x"', "tag:>", "plain:hi", "tag:</a", "tag:>"],
    ]);
  });

  it("doesn't color words inside longer names", () => {
    expect(kinds("returned item2", "js")).toEqual([["plain:returned item2"]]);
  });
});
