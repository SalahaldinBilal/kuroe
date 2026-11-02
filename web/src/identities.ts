import type { Identity, IdentityKind } from "./api";

export type Provider = "github" | "gitlab" | "gitea" | "git" | "registry" | "cloudflare" | "certificate";

export const PROVIDERS: { value: Provider; label: string; badge: string; description: string }[] = [
  { value: "github", label: "GitHub", badge: "GH", description: "Repos, push webhooks, ghcr.io" },
  { value: "gitlab", label: "GitLab", badge: "GL", description: "gitlab.com or self-hosted" },
  { value: "gitea", label: "Gitea or Forgejo", badge: "GT", description: "Self-hosted Git" },
  { value: "git", label: "Other Git host", badge: "G", description: "HTTPS username and token" },
  { value: "registry", label: "Container registry", badge: "R", description: "Docker Hub, ghcr.io, others" },
  { value: "cloudflare", label: "Cloudflare", badge: "CF", description: "DNS and origin certificates" },
  {
    value: "certificate",
    label: "TLS certificate",
    badge: "TLS",
    description: "Paste one, or get a Cloudflare origin one",
  },
];

export const KIND_LABEL: Record<IdentityKind, string> = {
  github_app: "GitHub App",
  github_token: "GitHub token",
  gitlab: "GitLab token",
  gitea: "Gitea token",
  git: "Git login",
  registry: "Registry login",
  cloudflare: "Cloudflare token",
  certificate: "Certificate",
};

export const BADGE: Record<IdentityKind, string> = {
  github_app: "GH",
  github_token: "GH",
  gitlab: "GL",
  gitea: "GT",
  git: "G",
  registry: "R",
  cloudflare: "CF",
  certificate: "TLS",
};

export type Group = "git" | "registry" | "dns" | "certs";

export function groupOf(kind: IdentityKind): Group {
  if (kind === "registry") return "registry";
  if (kind === "cloudflare") return "dns";
  if (kind === "certificate") return "certs";
  return "git";
}

export const clonesGit = (kind: IdentityKind) => groupOf(kind) === "git";
export const pullsImages = (kind: IdentityKind) => kind === "registry" || kind === "github_token" || kind === "gitlab";

const DAY = 24 * 60 * 60;

/** A warning when the provider said the token runs out within two weeks, or already did. */
export function expiryWarning(identity: Pick<Identity, "expires_at">, now = Date.now() / 1000): string | null {
  if (identity.expires_at == null) return null;
  const days = Math.floor((identity.expires_at - now) / DAY);
  if (days < 0) return "Expired";
  if (days > 14) return null;
  if (days === 0) return "Expires today";
  return `Expires in ${days} ${days === 1 ? "day" : "days"}`;
}

/** What a saved login reaches, in one short line. */
export function scopeLine(identity: Identity): string {
  const meta = identity.meta as Record<string, unknown>;
  switch (identity.kind) {
    case "github_app":
      return typeof meta.repositories === "number"
        ? `${meta.repositories} repositories in ${meta.owner ?? "GitHub"}`
        : meta.installation_id
          ? `installed for ${meta.owner ?? "GitHub"}`
          : "not installed yet";
    case "cloudflare": {
      const zones = Array.isArray(meta.zones) ? (meta.zones as string[]) : [];
      return zones.length
        ? `DNS on ${zones.slice(0, 3).join(", ")}${zones.length > 3 ? ` and ${zones.length - 3} more` : ""}`
        : "no zones";
    }
    case "certificate": {
      const sans = Array.isArray(meta.sans) ? (meta.sans as string[]) : [];
      const origin = meta.cloudflare_origin ? "Cloudflare origin, " : "";
      const pulls = meta.aop ? ", origin pulls on" : "";
      return `${origin}${sans.join(", ")}${pulls}`;
    }
    default:
      return (
        [identity.username && `user ${identity.username}`, identity.host].filter(Boolean).join(" on ") || "github.com"
      );
  }
}

/** Sends the browser to GitHub with the app manifest, the way GitHub's manifest flow expects. */
export function postManifest(action: string, manifest: string) {
  const form = document.createElement("form");
  form.method = "post";
  form.action = action;
  const field = document.createElement("input");
  field.type = "hidden";
  field.name = "manifest";
  field.value = manifest;
  form.append(field);
  document.body.append(form);
  form.submit();
}
