import type { Domain, ErrorPage, RouteOptions } from "./api";

export const MAX_ERROR_PAGE = 256 * 1024;

export interface PathDraft {
  path: string;
  exact: boolean;
  service: string;
  port: string;
  page: ErrorPage | null;
}

export interface RoutingDraft {
  kind: "service" | "redirect";
  service: string;
  port: string;
  redirect: string;
  compress: boolean;
  /** Comma separated, empty for most types. */
  compressTypes: string;
  compressMin: string;
  gzipOnly: boolean;
  /** null follows the error pages: off where one applies. */
  retry: boolean | null;
  timeout: string;
  maxBody: string;
  rewriteHost: boolean;
  page: ErrorPage | null;
  paths: PathDraft[];
}

export function emptyPath(service: string): PathDraft {
  return { path: "", exact: false, service, port: "", page: null };
}

export function routingDraft(domain: Domain | null, service: string, port: string): RoutingDraft {
  const o = domain?.options ?? {};
  return {
    kind: o.redirect ? "redirect" : "service",
    service: domain?.service || service,
    port: domain && domain.port > 0 ? String(domain.port) : port,
    redirect: o.redirect ?? "",
    compress: !!o.compress,
    compressTypes: (o.compress_types ?? []).join(", "),
    compressMin: o.compress_min_bytes != null ? String(o.compress_min_bytes) : "",
    gzipOnly: !!o.compress_gzip_only,
    retry: o.retry ?? null,
    timeout: o.timeout != null ? String(o.timeout) : "",
    maxBody: o.max_body_mb != null ? String(o.max_body_mb) : "",
    rewriteHost: !!o.rewrite_host,
    page: o.error_page ?? null,
    paths: (o.paths ?? []).map((p) => ({
      path: p.path,
      exact: !!p.exact,
      service: p.service,
      port: String(p.port),
      page: p.error_page ?? null,
    })),
  };
}

/** The service, port and options the API takes. A redirect drops everything else. */
export function routingOf(draft: RoutingDraft): { service: string; port: number; options: RouteOptions } {
  if (draft.kind === "redirect") return { service: "", port: 0, options: { redirect: draft.redirect.trim() } };
  const options: RouteOptions = {};
  if (draft.compress) {
    options.compress = true;
    const types = draft.compressTypes
      .split(",")
      .map((t) => t.trim())
      .filter(Boolean);
    if (types.length > 0) options.compress_types = types;
    if (draft.compressMin.trim()) options.compress_min_bytes = Number(draft.compressMin);
    if (draft.gzipOnly) options.compress_gzip_only = true;
  }
  if (draft.retry !== null) options.retry = draft.retry;
  if (draft.timeout.trim()) options.timeout = Number(draft.timeout);
  if (draft.maxBody.trim()) options.max_body_mb = Number(draft.maxBody);
  if (draft.rewriteHost) options.rewrite_host = true;
  if (draft.page) options.error_page = draft.page;
  const paths = draft.paths
    .filter((p) => p.path.trim())
    .map((p) => ({
      path: p.path.trim(),
      ...(p.exact ? { exact: true } : {}),
      service: p.service,
      port: Number(p.port),
      ...(p.page ? { error_page: p.page } : {}),
    }));
  if (paths.length > 0) options.paths = paths;
  return { service: draft.service, port: Number(draft.port), options };
}

/** Why the draft can't be saved yet, or null. */
export function routingProblem(draft: RoutingDraft, site: boolean): string | null {
  if (draft.kind === "redirect") return draft.redirect.trim() ? null : "Enter the address to redirect to.";
  if (!site && (!draft.service || !(Number(draft.port) > 0))) return "Pick a service and a port.";
  const bad = draft.paths.find((p) => p.path.trim() && (!p.service || !(Number(p.port) > 0)));
  if (bad) return `Pick a service and a port for ${bad.path.trim()}.`;
  const whole = (text: string) => !text.trim() || /^[1-9][0-9]*$/.test(text.trim());
  if (!whole(draft.timeout)) return "The timeout is a whole number of seconds.";
  if (!whole(draft.maxBody)) return "The body limit is a whole number of MB.";
  if (draft.compress && draft.compressMin.trim() && !/^[0-9]+$/.test(draft.compressMin.trim())) {
    return "The minimum size is a whole number of bytes.";
  }
  return null;
}

/** Whether a router with or without an error page retries, the way the server decides it. */
export function retries(draft: RoutingDraft, hasPage: boolean): boolean {
  return draft.retry ?? !hasPage;
}

/** An uploaded file as an error page. JSON by its name or type, HTML otherwise. */
export async function readErrorPage(file: File): Promise<ErrorPage> {
  if (file.size > MAX_ERROR_PAGE) throw new Error(`${file.name} is bigger than ${MAX_ERROR_PAGE / 1024} KB.`);
  const body = await file.text();
  if (!body.trim()) throw new Error(`${file.name} is empty.`);
  const json = file.type === "application/json" || file.name.toLowerCase().endsWith(".json");
  return json ? { body, json: true } : { body };
}

export function pageLabel(page: ErrorPage): string {
  const kb = Math.max(1, Math.round(new Blob([page.body]).size / 1024));
  return `${page.json ? "JSON" : "HTML"}, ${kb} KB`;
}

/** A short line for the domains table, like "2 paths, compressed". Empty when there's nothing extra. */
export function routingSummary(options: RouteOptions): string {
  const parts: string[] = [];
  const paths = options.paths?.length ?? 0;
  if (paths > 0) parts.push(`${paths} ${paths === 1 ? "path" : "paths"}`);
  if (options.compress) parts.push("compressed");
  if (options.error_page || options.paths?.some((p) => p.error_page)) parts.push("error page");
  if (options.timeout) parts.push(`${options.timeout} s timeout`);
  if (options.max_body_mb) parts.push(`${options.max_body_mb} MB limit`);
  if (options.rewrite_host) parts.push("own Host");
  return parts.join(", ");
}
