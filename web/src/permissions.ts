import type { Permission, Scope } from "./api";

export const PERMISSION_GROUPS: [string, [Permission, string][]][] = [
  [
    "Deployments",
    [
      ["dep.view", "View deployments and logs"],
      ["dep.create", "Create deployments"],
      ["dep.deploy", "Deploy, restart and stop"],
      ["dep.edit", "Change settings and environment"],
      ["dep.secrets", "See secret values"],
      ["dep.shell", "Open shell, attach and files"],
      ["dep.domains", "Manage domains"],
      ["dep.ci", "Manage deploy tokens and webhooks"],
      ["dep.delete", "Delete deployments"],
    ],
  ],
  [
    "Server",
    [
      ["vol", "Manage shared volumes"],
      ["proxy", "Change proxy settings"],
      ["ident.use", "Use identities on deployments"],
      ["ident.manage", "Add and remove identities"],
      ["secret.use", "Use saved secrets on deployments"],
      ["secret.manage", "Add, change and reveal saved secrets"],
      ["settings", "Change server settings"],
      ["import", "Import"],
      ["users", "Manage users and roles"],
      ["runners", "Manage CI runners"],
    ],
  ],
];

export const ALL_PERMISSIONS: Permission[] = PERMISSION_GROUPS.flatMap(([, list]) => list.map(([id]) => id));

export function scopeLabel(scope: Scope, names: Map<string, string>): string {
  if (scope === "all") return "All deployments";
  if (scope === "granted") return "Deployments they're granted";
  if (scope.length === 0) return "No deployments";
  return scope.map((id) => names.get(id) ?? "deleted").join(", ");
}

export function togglePermission(list: Permission[], permission: Permission, on: boolean): Permission[] {
  const without = list.filter((p) => p !== permission);
  return on ? ALL_PERMISSIONS.filter((p) => p === permission || without.includes(p)) : without;
}
