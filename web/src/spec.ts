import { GIT_TOKEN_REF, type PortMapping, type SharedMount, type SiteOptions, type Spec } from "./api";

export interface PortRow {
  host: string;
  container: string;
  protocol: "tcp" | "udp";
}

export interface EnvRow {
  key: string;
  value: string;
  secret: boolean;
  /** A saved secret's id. When set, the row has no value of its own. */
  ref: string | null;
}

export interface MountRow {
  volume: string;
  target: string;
  readOnly: boolean;
}

export const emptyPort = (): PortRow => ({ host: "", container: "", protocol: "tcp" });
export const emptyEnv = (): EnvRow => ({ key: "", value: "", secret: false, ref: null });

function port(text: string, line: string): number {
  const n = Number(text.trim());
  if (!Number.isInteger(n) || n < 1 || n > 65535) throw new Error(`"${line}" is not a valid port`);
  return n;
}

export function portsToRows(ports: PortMapping[]): PortRow[] {
  return ports.map((p) => ({
    host: p.host ? String(p.host) : "",
    container: String(p.container),
    protocol: p.protocol,
  }));
}

export function rowsToPorts(rows: PortRow[]): PortMapping[] {
  return rows
    .filter((r) => r.host.trim() || r.container.trim())
    .map((r) => {
      const label = r.host.trim() ? `${r.host}:${r.container}` : r.container;
      if (!r.container.trim()) throw new Error(`port ${r.host} needs a container port`);
      return {
        host: r.host.trim() ? port(r.host, label) : null,
        container: port(r.container, label),
        protocol: r.protocol,
      };
    });
}

export function envToRows(
  env: Record<string, string>,
  secrets: string[] = [],
  refs: Record<string, string> = {},
): EnvRow[] {
  return Object.entries(env).map(([key, value]) => {
    const ref = refs[key] ?? null;
    return { key, value: ref ? "" : value, secret: !ref && secrets.includes(key), ref };
  });
}

export function rowsToEnv(rows: EnvRow[]): {
  env: Record<string, string>;
  secrets: string[];
  refs: Record<string, string>;
} {
  const env: Record<string, string> = {};
  const secrets: string[] = [];
  const refs: Record<string, string> = {};
  for (const row of rows) {
    const key = row.key.trim();
    if (!key && !row.value && !row.ref) continue;
    if (!key) throw new Error("an environment value has no key");
    if (key in env) throw new Error(`${key} is set twice`);
    if (row.ref) {
      env[key] = "";
      refs[key] = row.ref;
    } else {
      env[key] = row.value;
      if (row.secret) secrets.push(key);
    }
  }
  return { env, secrets, refs };
}

/** The spec's saved secret references with the env ones replaced, keeping the git token's. */
export function withEnvRefs(spec: Spec | null, refs: Record<string, string>): Record<string, string> {
  const token = spec?.secret_refs?.[GIT_TOKEN_REF];
  return token ? { ...refs, [GIT_TOKEN_REF]: token } : refs;
}

export function mountsToRows(mounts: SharedMount[]): MountRow[] {
  return mounts.map((m) => ({ volume: m.volume, target: m.target, readOnly: m.read_only }));
}

export function rowsToMounts(rows: MountRow[]): SharedMount[] {
  return rows
    .filter((r) => r.target.trim())
    .map((r) => {
      if (!r.volume) throw new Error(`pick a volume for ${r.target}`);
      if (!r.target.trim().startsWith("/")) throw new Error(`mount path "${r.target}" must start with /`);
      return { volume: r.volume, target: r.target.trim(), read_only: r.readOnly };
    });
}

export const mbitText = (v: number | null | undefined) => (v == null ? "" : String(v));

export function parseMbit(text: string, what: string): number | null {
  const value = text.trim();
  if (!value) return null;
  const mbit = Number(value);
  if (!(mbit > 0) || !Number.isFinite(mbit)) throw new Error(`${what} must be a positive number of Mbit/s`);
  return mbit;
}

/** Compose source, or a git repository deployed from its own Compose file. */
export function isCompose(spec: Spec): boolean {
  return spec.source.kind === "compose" || (spec.source.kind === "git" && spec.source.compose_file != null);
}

/** Files served by the shared static server, from a git repository, an image or an uploaded archive. */
export function isStatic(spec: Spec): boolean {
  if (!spec.site) return false;
  const s = spec.source;
  return s.kind === "static" || s.kind === "image" || (s.kind === "git" && s.compose_file == null);
}

export type MimeRow = { key: string; value: string };

export function mimeRows(types: Record<string, string> | undefined): MimeRow[] {
  return Object.entries(types ?? {}).map(([key, value]) => ({ key, value }));
}

/** Extension to content type. Extensions lose a leading dot and go lowercase, rows with both fields empty are skipped. */
export function mimeTypesOf(rows: MimeRow[]): Record<string, string> {
  const types: Record<string, string> = {};
  for (const row of rows) {
    const ext = row.key.trim().replace(/^\.+/, "").toLowerCase();
    const type = row.value.trim();
    if (!ext && !type) continue;
    if (!ext || !type) throw new Error("Each file type needs an extension and a content type");
    if (ext in types) throw new Error(`.${ext} is listed twice`);
    types[ext] = type;
  }
  return types;
}

export interface SiteDraft {
  build: "none" | "command" | "dockerfile";
  image: string;
  command: string;
  dockerfile: string;
  output: string;
  spa: boolean;
  types: MimeRow[];
}

export const NODE_SITE: SiteDraft = {
  build: "command",
  image: "node:26-alpine",
  command: "npm ci && npm run build",
  dockerfile: "",
  output: "dist",
  spa: true,
  types: [],
};

export function siteDraft(site: SiteOptions | null | undefined): SiteDraft {
  if (!site) return { ...NODE_SITE };
  const b = site.build;
  return {
    build: b.kind,
    image: b.kind === "command" ? b.image : NODE_SITE.image,
    command: b.kind === "command" ? b.command : NODE_SITE.command,
    dockerfile: b.kind === "dockerfile" ? (b.dockerfile ?? "") : "",
    output: site.output,
    spa: site.spa,
    types: mimeRows(site.mime_types),
  };
}

function buildOf(d: SiteDraft, output: string): SiteOptions["build"] {
  switch (d.build) {
    case "command":
      if (!d.image.trim() || !d.command.trim()) throw new Error("the build step needs an image and a command");
      return { kind: "command", image: d.image.trim(), command: d.command.trim() };
    case "dockerfile":
      if (!output.startsWith("/"))
        throw new Error("with a Dockerfile, the output folder is a path in the image, like /app/dist");
      return { kind: "dockerfile", dockerfile: d.dockerfile.trim() || null };
    default:
      return { kind: "none" };
  }
}

export function siteOf(d: SiteDraft): SiteOptions {
  const output = d.output.trim();
  const site: SiteOptions = { build: buildOf(d, output), output, spa: d.spa };
  const types = mimeTypesOf(d.types);
  if (Object.keys(types).length) site.mime_types = types;
  return site;
}

export function sourceLabel(spec: Spec): string {
  const s = spec.source;
  switch (s.kind) {
    case "image":
      return spec.site ? `${s.image} · static site from ${spec.site.output}` : s.image;
    case "git": {
      const extra =
        s.compose_file != null ? ` · ${s.compose_file || "Compose file"}` : spec.site ? " · static site" : "";
      return `${s.url}${s.reference ? `#${s.reference}` : ""}${extra}`;
    }
    case "upload":
      return "uploaded build context";
    case "compose":
      return "compose.yaml";
    case "dockerfile":
      return "Dockerfile written here";
    case "static":
      return "static site from an uploaded archive";
  }
}
