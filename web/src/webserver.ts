import type { WebServer, WebServerKind } from "./api";

interface ServerInfo {
  label: string;
  passthrough: boolean;
  ownCertificates: boolean;
  path: string;
  extraPath: string | null;
  command: string;
}

// Same defaults as `Kind` in src/proxy/webserver/mod.rs.
export const WEB_SERVERS: Record<WebServerKind, ServerInfo> = {
  nginx: {
    label: "nginx",
    passthrough: true,
    ownCertificates: false,
    path: "/etc/nginx/conf.d/kuroe.conf",
    extraPath: "/etc/nginx/kuroe-stream.conf",
    command: "nginx -t && nginx -s reload",
  },
  apache: {
    label: "Apache",
    passthrough: false,
    ownCertificates: false,
    path: "/etc/apache2/sites-enabled/kuroe.conf",
    extraPath: null,
    command: "apachectl configtest && apachectl graceful",
  },
  caddy: {
    label: "Caddy",
    passthrough: false,
    ownCertificates: true,
    path: "/etc/caddy/kuroe.caddy",
    extraPath: null,
    command: "caddy validate --config /etc/caddy/Caddyfile && caddy reload --config /etc/caddy/Caddyfile",
  },
  haproxy: {
    label: "HAProxy",
    passthrough: true,
    ownCertificates: true,
    path: "/etc/haproxy/conf.d/kuroe.cfg",
    extraPath: "/etc/haproxy/kuroe.map",
    command: "haproxy -c -f /etc/haproxy/haproxy.cfg -f /etc/haproxy/conf.d && systemctl reload haproxy",
  },
  traefik: {
    label: "Traefik",
    passthrough: true,
    ownCertificates: true,
    path: "/etc/traefik/dynamic/kuroe.yml",
    extraPath: null,
    command: "",
  },
  lighttpd: {
    label: "lighttpd",
    passthrough: false,
    ownCertificates: false,
    path: "/etc/lighttpd/conf-enabled/90-kuroe.conf",
    extraPath: null,
    command: "lighttpd -tt -f /etc/lighttpd/lighttpd.conf && systemctl reload lighttpd",
  },
};

export const KINDS = Object.keys(WEB_SERVERS) as WebServerKind[];

/** The web server settings as the form edits them: no nulls, and where it runs as its own field. */
export interface WebServerDraft {
  kind: WebServerKind;
  mode: "front" | "passthrough";
  path: string;
  extra_path: string;
  where: "host" | "container";
  command: string;
  container: string;
  auto: boolean;
  cert: string;
  key: string;
  rest_tls: string;
}

export function serverDefaults(kind: WebServerKind, container: string | null = null): WebServerDraft {
  const info = WEB_SERVERS[kind];
  return {
    kind,
    mode: "front",
    path: info.path,
    extra_path: info.extraPath ?? "",
    where: container ? "container" : "host",
    command: info.command,
    container: container ?? "",
    auto: true,
    cert: "/etc/letsencrypt/live/{host}/fullchain.pem",
    key: "/etc/letsencrypt/live/{host}/privkey.pem",
    rest_tls: "127.0.0.1:4443",
  };
}

/** Picking another server keeps where it runs and the auto switch, and resets the paths to the new server's. */
export function switchKind(draft: WebServerDraft, kind: WebServerKind): WebServerDraft {
  const next = serverDefaults(kind, draft.where === "container" ? draft.container : null);
  return { ...next, auto: draft.auto, mode: WEB_SERVERS[kind].passthrough ? draft.mode : "front" };
}

export function serverDraft(s: WebServer): WebServerDraft {
  return {
    kind: s.kind,
    mode: s.mode,
    path: s.path,
    extra_path: s.extra_path ?? "",
    where: s.container ? "container" : "host",
    command: s.command,
    container: s.container ?? "",
    auto: s.auto,
    cert: s.cert_paths.cert,
    key: s.cert_paths.key,
    rest_tls: s.rest_tls,
  };
}

export function serverOf(d: WebServerDraft): WebServer {
  return {
    kind: d.kind,
    mode: WEB_SERVERS[d.kind].passthrough ? d.mode : "front",
    path: d.path.trim(),
    extra_path: d.extra_path.trim() || null,
    command: d.command.trim(),
    container: d.where === "container" ? d.container.trim() || null : null,
    auto: d.auto,
    cert_paths: { cert: d.cert.trim(), key: d.key.trim() },
    rest_tls: d.rest_tls.trim(),
  };
}

/** A second file: nginx's stream part with passthrough, HAProxy's hostname map always. */
export function extraFile(kind: WebServerKind, mode: "front" | "passthrough"): string | null {
  if (kind === "haproxy") return "Hostname map file";
  if (kind === "nginx" && mode === "passthrough") return "Stream config file";
  return null;
}
