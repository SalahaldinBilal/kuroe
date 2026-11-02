import type { Identity, ServedBy } from "./api";

/** `*.example.com` covers exactly one label below it, never the apex or two levels down. Same rule as the server. */
export function covers(san: string, host: string): boolean {
  const h = host.toLowerCase();
  if (!san.startsWith("*.")) return san === h;
  const base = san.slice(2);
  if (!h.endsWith(`.${base}`)) return false;
  const label = h.slice(0, h.length - base.length - 1);
  return label.length > 0 && !label.includes(".");
}

export function sansOf(identity: Identity): string[] {
  const sans = identity.meta.sans;
  return Array.isArray(sans) ? (sans as string[]) : [];
}

export function certificatesFor(all: Identity[], host: string): Identity[] {
  return all.filter((i) => i.kind === "certificate" && sansOf(i).some((s) => covers(s, host)));
}

export function servedLabel(served: ServedBy): string {
  const how = served.pinned ? "pinned" : "automatic";
  switch (served.kind) {
    case "saved":
      return `${served.name}, ${how}`;
    case "acme":
      return served.pinned ? "Let's Encrypt, pinned" : "Let's Encrypt";
    case "server":
      return "Your web server";
    default:
      return "Traefik default";
  }
}

/** The usual hostnames for one zone: the apex and every first-level subdomain. */
export function zoneHostnames(zone: string): string[] {
  return zone ? [zone, `*.${zone}`] : [];
}
