export interface Pair {
  key: string;
  value: string;
}

export type BodyMode = "none" | "raw" | "json" | "form";
export type BodyKind = "html" | "json" | "image" | "video" | "audio" | "pdf" | "text" | "binary";
export type Language = "xml" | "css" | "js" | "json" | "plain";

export const METHODS = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"] as const;
export const HISTORY_SIZE = 20;
const SNIFF_BYTES = 1024;

export function emptyPair(): Pair {
  return { key: "", value: "" };
}

/** Adds the filled-in params to the path's own query string. */
export function withQuery(path: string, params: Pair[]): string {
  const at = path.indexOf("?");
  const base = at === -1 ? path : path.slice(0, at);
  const search = new URLSearchParams(at === -1 ? "" : path.slice(at + 1));
  for (const { key, value } of params) {
    if (key.trim()) search.append(key.trim(), value);
  }
  const query = search.toString();
  return query ? `${base}?${query}` : base;
}

export function toBase64(bytes: Uint8Array): string {
  let binary = "";
  const step = 0x8000;
  for (let i = 0; i < bytes.length; i += step) {
    binary += String.fromCharCode(...bytes.subarray(i, i + step));
  }
  return btoa(binary);
}

export function fromBase64(text: string): Uint8Array<ArrayBuffer> {
  const binary = atob(text);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

export interface EncodedBody {
  body: string | null;
  contentType: string | null;
  error?: string;
}

/** The body to send and the Content-Type it goes with. */
export function encodeBody(mode: BodyMode, text: string, rawType: string, form: Pair[]): EncodedBody {
  const utf8 = (value: string) => toBase64(new TextEncoder().encode(value));
  if (mode === "none") return { body: null, contentType: null };
  if (mode === "json") {
    try {
      JSON.parse(text);
    } catch (err) {
      return { body: null, contentType: null, error: `The JSON body isn't valid: ${(err as Error).message}` };
    }
    return { body: utf8(text), contentType: "application/json" };
  }
  if (mode === "form") {
    const search = new URLSearchParams();
    for (const { key, value } of form) {
      if (key.trim()) search.append(key.trim(), value);
    }
    return { body: utf8(search.toString()), contentType: "application/x-www-form-urlencoded" };
  }
  return { body: utf8(text), contentType: rawType.trim() || "text/plain" };
}

/** The headers to send: the filled-in ones, plus the body's Content-Type unless one is set. */
export function outgoingHeaders(headers: Pair[], contentType: string | null): [string, string][] {
  const out: [string, string][] = headers.filter((h) => h.key.trim()).map((h) => [h.key.trim(), h.value]);
  if (contentType && !out.some(([key]) => key.toLowerCase() === "content-type"))
    out.push(["Content-Type", contentType]);
  return out;
}

export function headerValue(headers: [string, string][], name: string): string | undefined {
  return headers.find(([key]) => key.toLowerCase() === name.toLowerCase())?.[1];
}

function mimeOf(contentType: string | undefined): string {
  return (contentType ?? "").split(";")[0].trim().toLowerCase();
}

function looksLikeText(bytes: Uint8Array): boolean {
  const head = bytes.subarray(0, SNIFF_BYTES);
  if (head.includes(0)) return false;
  try {
    new TextDecoder("utf-8", { fatal: true }).decode(head.length < bytes.length ? trimPartial(head) : head);
    return true;
  } catch {
    return false;
  }
}

/** Drops a character cut in half at the end of a sample. */
function trimPartial(bytes: Uint8Array): Uint8Array {
  let end = bytes.length;
  for (let back = 1; back <= 3 && end - back >= 0; back++) {
    const byte = bytes[end - back];
    if ((byte & 0xc0) !== 0x80) {
      const needed = byte >= 0xf0 ? 4 : byte >= 0xe0 ? 3 : byte >= 0xc0 ? 2 : 1;
      if (needed > back) end -= back;
      break;
    }
  }
  return bytes.subarray(0, end);
}

/** How to show a body: from its Content-Type, or by looking at it when the type says nothing useful. */
export function bodyKind(contentType: string | undefined, bytes: Uint8Array): BodyKind {
  const mime = mimeOf(contentType);
  if (mime === "text/html" || mime === "application/xhtml+xml") return "html";
  if (mime === "application/json" || mime.endsWith("+json")) return "json";
  if (mime.startsWith("image/")) return "image";
  if (mime.startsWith("video/")) return "video";
  if (mime.startsWith("audio/")) return "audio";
  if (mime === "application/pdf") return "pdf";
  if (mime.startsWith("text/") || /(xml|javascript|ecmascript|css)$/.test(mime)) return "text";
  if (bytes.length === 0) return "text";
  return looksLikeText(bytes) ? "text" : "binary";
}

export function languageOf(contentType: string | undefined): Language {
  const mime = mimeOf(contentType);
  if (mime.includes("html") || mime.endsWith("xml")) return "xml";
  if (mime === "text/css") return "css";
  if (/(javascript|ecmascript)$/.test(mime)) return "js";
  if (mime === "application/json" || mime.endsWith("+json")) return "json";
  return "plain";
}

export function hexDump(bytes: Uint8Array, limit: number): string[] {
  const lines: string[] = [];
  const end = Math.min(bytes.length, limit);
  for (let offset = 0; offset < end; offset += 16) {
    const row = bytes.subarray(offset, Math.min(offset + 16, end));
    const hex = Array.from(row, (b) => b.toString(16).padStart(2, "0")).join(" ");
    const text = Array.from(row, (b) => (b >= 0x20 && b < 0x7f ? String.fromCharCode(b) : ".")).join("");
    lines.push(`${offset.toString(16).padStart(8, "0")}  ${hex.padEnd(47)}  |${text}|`);
  }
  return lines;
}

/** Points relative links in a page at the preview proxy, so its styles and images load in the rendered view. */
export function withBase(html: string, href: string): string {
  const tag = `<base href="${href.replace(/"/g, "&quot;")}">`;
  const head = /<head(\s[^>]*)?>/i.exec(html);
  if (head) return html.slice(0, head.index + head[0].length) + tag + html.slice(head.index + head[0].length);
  return tag + html;
}

/** The folder of a path, for the base of a rendered page. `/shop/cart?x=1` gives `/shop/`. */
export function folderOf(path: string): string {
  const clean = path.split(/[?#]/)[0];
  return clean.slice(0, clean.lastIndexOf("/") + 1) || "/";
}

export function statusTone(status: number): "ok" | "warn" | "bad" | undefined {
  if (status >= 500) return "bad";
  if (status >= 400) return "warn";
  if (status >= 200 && status < 300) return "ok";
  return undefined;
}

export interface SavedRequest {
  container: string;
  port: number;
  method: string;
  path: string;
  params: Pair[];
  headers: Pair[];
  mode: BodyMode;
  text: string;
  rawType: string;
  form: Pair[];
  status?: number;
  at: number;
}

function historyKey(deployment: string): string {
  return `kuroe.preview.${deployment}`;
}

export function loadHistory(deployment: string): SavedRequest[] {
  try {
    const saved = sessionStorage.getItem(historyKey(deployment));
    return saved ? (JSON.parse(saved) as SavedRequest[]) : [];
  } catch {
    return [];
  }
}

function sameRequest(a: SavedRequest, b: SavedRequest): boolean {
  const what = (r: SavedRequest) => JSON.stringify({ ...r, status: undefined, at: undefined });
  return what(a) === what(b);
}

/** Puts the request first, drops an identical older one, and keeps the newest `HISTORY_SIZE`. */
export function addToHistory(list: SavedRequest[], entry: SavedRequest): SavedRequest[] {
  return [entry, ...list.filter((saved) => !sameRequest(saved, entry))].slice(0, HISTORY_SIZE);
}

export function saveHistory(deployment: string, list: SavedRequest[]) {
  try {
    sessionStorage.setItem(historyKey(deployment), JSON.stringify(list));
  } catch {
    // A full or blocked session storage only loses the history.
  }
}
