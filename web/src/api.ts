export type Permission =
  | "dep.view"
  | "dep.create"
  | "dep.deploy"
  | "dep.edit"
  | "dep.secrets"
  | "dep.shell"
  | "dep.domains"
  | "dep.ci"
  | "dep.delete"
  | "vol"
  | "proxy"
  | "ident.use"
  | "ident.manage"
  | "secret.use"
  | "secret.manage"
  | "settings"
  | "import"
  | "users"
  | "runners";

export interface User {
  id: string;
  username: string;
  role: string;
  created_at: number;
  permissions: Permission[];
  show_http_banner?: boolean;
}

export interface UserRow {
  id: string;
  username: string;
  role: string;
  created_at: number;
  host_mounts: boolean;
  grants: number;
}

export function can(user: User, permission: Permission): boolean {
  return user.permissions.includes(permission);
}

export const isOwner = (user: { role: string }) => user.role === "owner";

export type Scope = "all" | "granted" | string[];

export interface RoleDef {
  name: string;
  builtin: boolean;
  description: string;
  permissions: Permission[];
  scope: Scope;
  users: number;
}

export interface RoleInput {
  name: string;
  description: string;
  permissions: Permission[];
  scope: Scope;
}

export type IdentityKind =
  | "github_app"
  | "github_token"
  | "gitlab"
  | "gitea"
  | "git"
  | "registry"
  | "cloudflare"
  | "certificate";

export interface Identity {
  id: string;
  kind: IdentityKind;
  name: string;
  host: string;
  username: string;
  meta: Record<string, unknown>;
  created_at: number;
  last_used_at: number | null;
  expires_at: number | null;
  used_by: { id: string; name: string }[];
}

export interface NewIdentity {
  kind: IdentityKind;
  name: string;
  host: string;
  username: string;
  secret: string;
}

export interface Repo {
  full_name: string;
  clone_url: string;
  default_branch: string;
  private: boolean;
}

export interface Grant {
  deployment_id: string;
  role: string;
}

export interface Reach {
  user_id: string;
  username: string;
  role: string;
  via: "owner" | "grant" | "role";
}

export interface DaemonInfo {
  server_version: string | null;
  api_version: string | null;
  operating_system: string | null;
  architecture: string | null;
  cpus: number | null;
  memory_bytes: number | null;
  containers: number | null;
  containers_running: number | null;
  images: number | null;
  swarm: string | null;
  swarm_manager: boolean;
}

export interface PoolUsage {
  cpus: number;
  memory_bytes: number;
  storage_bytes: number | null;
  cpus_used: number;
  memory_used: number;
  cpus_counted: number;
  memory_counted: number;
  storage_used: number;
  cpus_actual: number;
  memory_actual: number;
  disk_used: number | null;
  disk_total: number | null;
  containers: number;
  cpus_unlimited: number;
  memory_unlimited: number;
}

export type Source =
  | { kind: "image"; image: string; registry_identity?: string | null }
  | {
      kind: "git";
      url: string;
      reference?: string | null;
      context?: string | null;
      dockerfile?: string | null;
      token?: string | null;
      identity?: string | null;
      compose_file?: string | null;
    }
  | { kind: "upload"; dockerfile?: string | null }
  | { kind: "compose"; file: string }
  | { kind: "dockerfile"; file: string }
  | { kind: "static" };

export type SiteBuild =
  | { kind: "none" }
  | { kind: "command"; image: string; command: string }
  | { kind: "dockerfile"; dockerfile?: string | null };

export interface SiteOptions {
  build: SiteBuild;
  output: string;
  spa: boolean;
}

export type SourceKind = Source["kind"];

export interface PortMapping {
  container: number;
  host?: number | null;
  protocol: "tcp" | "udp";
}

export interface SharedMount {
  volume: string;
  target: string;
  read_only: boolean;
}

export interface ServiceSettings {
  public: boolean;
  cpus?: number | null;
  memory_mb?: number | null;
  pids?: number | null;
  net_in_mbit?: number | null;
  net_out_mbit?: number | null;
  net_conn_out_mbit?: number | null;
  volumes: SharedMount[];
}

export type RunMode = "compose" | "stack";

export interface Spec {
  source: Source;
  container: { ports: PortMapping[]; env: Record<string, string>; command?: string[] | null };
  services: Record<string, ServiceSettings>;
  mode?: RunMode;
  secrets?: string[];
  /** Env keys, and GIT_TOKEN_REF for the git token, that use a saved secret, by its id. */
  secret_refs?: Record<string, string>;
  site?: SiteOptions | null;
  host_access?: boolean;
}

export const GIT_TOKEN_REF = "@git_token";

export interface SavedSecret {
  id: string;
  name: string;
  description: string;
  created_at: number;
  updated_at: number;
  used_by: { id: string; name: string }[];
}

export interface DeployToken {
  id: string;
  name: string;
  host_mounts: boolean;
  created_at: number;
  last_used_at: number | null;
}

export interface ImportChange {
  name: string;
  action: "create" | "update" | "unchanged";
  fields: string[];
}

export interface ImportPlan {
  deployments: ImportChange[];
  volumes: string[];
  settings: { key: string; from: string | null; to: string }[];
  roles: ImportChange[];
  identities: ImportChange[];
  users: ImportChange[];
  runner_pools: ImportChange[];
  secrets: { key: string; label: string }[];
  webhooks: string[];
  tokens: { deployment: string; name: string }[];
}

export interface ImportResult {
  deployments: string[];
  volumes_to_create: string[];
  webhooks: {
    deployment: string;
    deployment_id: string;
    secret: string;
    setup?: { urls: HookUrls | null; provider: HookProvider; can_install: boolean };
  }[];
  tokens: { deployment: string; deployment_id: string; name: string; token: string }[];
}

export type Status = "created" | "deploying" | "running" | "degraded" | "down" | "failed" | "stopped";

export interface Deployment {
  id: string;
  name: string;
  spec: Spec;
  status: Status;
  revision: number;
  last_output: string;
  created_by: string | null;
  created_at: number;
  updated_at: number;
}

export type DeployStatus = "deploying" | "running" | "failed" | "cancelled";

export type DeployStepName =
  | "resolving_secrets"
  | "cloning"
  | "checking_pool"
  | "building"
  | "pulling"
  | "starting"
  | "health_check"
  | "switching_traffic"
  | "draining"
  | "stopping_old"
  | "publishing_routes"
  | "installing_files"
  | "restoring_routes"
  | "stopping_new"
  | "cleaning_up";

export interface DeployStep {
  step: DeployStepName;
  detail?: string;
  /** null while the step is still to come. */
  started_ms: number | null;
  finished_ms: number | null;
  failed?: boolean;
  /** Index of the first output line of this step. Missing on older deploys. */
  first_line?: number;
}

export interface DeploySource {
  kind: string;
  url?: string;
  provider?: "github" | "gitlab" | "gitea";
  reference?: string;
  commit?: string;
  image?: string;
  digest?: string;
}

export interface DeploySummary {
  id: string;
  revision: number;
  status: DeployStatus;
  trigger: "ui" | "deploy_token" | "webhook";
  triggered_by: string;
  user_id: string | null;
  token_id: string | null;
  source: DeploySource;
  started_at: number;
  finished_at: number | null;
}

export interface DeployDetail extends DeploySummary {
  output: string;
  steps: DeployStep[];
}

export type LiveDeployMessage =
  | { kind: "snapshot"; deploy: string | null; lines: string[]; partial: string | null; steps: DeployStep[] }
  | { kind: "line"; text: string }
  | { kind: "partial"; text: string | null }
  | { kind: "steps"; steps: DeployStep[] };

export interface PreviewRequest {
  port: number;
  method: string;
  /** With the query string. */
  path: string;
  headers: [string, string][];
  /** Base64. */
  body: string | null;
}

export interface PreviewAnswer {
  status: number;
  reason: string | null;
  headers: [string, string][];
  /** Base64, cut off at 16 MiB. */
  body: string;
  size: number;
  truncated: boolean;
  time_ms: number;
}

export interface DeploymentRow extends Deployment {
  /** The step a running deploy is in. */
  step: DeployStep | null;
}

export interface ContainerRow {
  id: string;
  name: string;
  service: string | null;
  image: string | null;
  state: string | null;
  status: string | null;
  ports: string[];
  private_ports: number[];
}

export interface DeploymentDetail extends Deployment {
  containers: ContainerRow[];
  permissions: Permission[];
  strategy: "blue_green" | "recreate" | "stack" | "static";
  strategy_note: string | null;
  active_project: string | null;
  /** Saved secret names by id, for the references in the spec. */
  secret_names: Record<string, string>;
  /** The newest deploy in the history. */
  last_deploy: string | null;
}

export type ProxyMode = "off" | "managed" | "external";

export interface ProxySettings {
  mode: ProxyMode;
  acme_email: string | null;
  http_port: number;
  https_port: number;
  drain_seconds: number;
  cert_mode: CertMode;
  dns_identity: string | null;
  dns_wildcard: boolean;
  bind: "ports" | "behind";
  behind_http: string;
  behind_https: string;
  web_server: WebServer | null;
  access_log?: boolean;
}

export type WebServerKind = "nginx" | "apache" | "caddy" | "haproxy" | "traefik" | "lighttpd";

export interface WebServer {
  kind: WebServerKind;
  mode: "front" | "passthrough";
  path: string;
  extra_path: string | null;
  command: string;
  container: string | null;
  auto: boolean;
  cert_paths: { cert: string; key: string };
  rest_tls: string;
}

export interface PortHolder {
  port: number;
  by: WebServerKind | null;
  what: string;
  container: string | null;
  how: string;
}

export interface ConfigFile {
  path: string;
  body: string;
}

export interface WebServerPreview {
  files: ConfigFile[];
  steps: string[];
  skipped: string[];
}

export type CertMode = "http" | "dns" | "none";

export interface ServedBy {
  kind: "acme" | "saved" | "default" | "server";
  name: string | null;
  pinned: boolean;
}

export interface Attention {
  kind: string;
  message: string;
  href: string;
}

export interface Domain {
  id: string;
  deployment_id: string;
  hostname: string;
  service: string;
  port: number;
  tls: boolean;
  probe_token: string;
  verified_at: number | null;
  created_at: number;
  certificate: string | null;
  options: RouteOptions;
  served_by: ServedBy;
}

export interface ErrorPage {
  body: string;
  json?: boolean;
}

export interface PathRule {
  path: string;
  exact?: boolean;
  service: string;
  port: number;
  error_page?: ErrorPage;
}

/** What a domain does beyond sending everything to one service. Managed Traefik applies it. */
export interface RouteOptions {
  redirect?: string;
  compress?: boolean;
  compress_types?: string[];
  compress_min_bytes?: number;
  compress_gzip_only?: boolean;
  /** Unset: on, except where an error page applies. */
  retry?: boolean;
  timeout?: number;
  max_body_mb?: number;
  rewrite_host?: boolean;
  paths?: PathRule[];
  error_page?: ErrorPage;
}

export interface Snippet {
  kind: WebServerKind;
  label: string;
  files: ConfigFile[];
  steps: string[];
}

export type CiPlatform = "github" | "gitlab" | "gitea" | "forgejo" | "shell";

export interface Pipeline {
  platform: CiPlatform;
  path: string;
  body: string;
  secrets: string[];
  notes: string[];
}

export type HookProvider = "github" | "gitlab" | "gitea";
export type HookUrls = Record<HookProvider, string>;

export interface WebhookInfo {
  enabled: boolean;
  urls: HookUrls | null;
  provider: HookProvider;
  app: string | null;
  can_install: boolean;
}

export type RunnerProvider = "github" | "gitlab" | "gitea";
export type RunnerScope = "repo" | "org" | "instance";
export type RunnerDocker = "dind" | "host" | "none";

export interface RunnerInput {
  name: string;
  provider: RunnerProvider;
  identity: string;
  scope_kind: RunnerScope;
  scope: string;
  labels: string[];
  min_idle: number;
  max: number;
  cpus: number | null;
  memory_mb: number | null;
  net_in_mbit: number | null;
  net_out_mbit: number | null;
  docker_mode: RunnerDocker;
  image: string | null;
  enabled: boolean;
}

export interface RunnerInstance {
  name: string;
  container: string;
  state: string;
  status: string;
  remote_id: string | null;
  created: number;
  busy: boolean | null;
}

export interface RunnerPool extends RunnerInput {
  id: string;
  last_error: string | null;
  created_by: string | null;
  created_at: number;
  instances: RunnerInstance[];
  default_image: string;
}

export interface VolumeLimit {
  max_bytes: number;
  stop: boolean;
}

export interface VolumeInfo {
  /** The Docker volume name. */
  name: string;
  shared: string | null;
  deployments: { id: string; name: string }[];
  bytes: number | null;
  measured_at: number | null;
  limit: VolumeLimit | null;
  over: boolean;
}

export interface Volume {
  name: string;
  created_by: string | null;
  created_at: number;
  usage: VolumeInfo | null;
}

export interface FileEntry {
  name: string;
  kind: "dir" | "file" | "link" | "other";
  size: number;
  mode: number;
  modified: number;
  link: string | null;
  link_dir?: boolean;
}

export interface Listing {
  entries: FileEntry[];
  note: string | null;
}

export interface FolderSizes {
  sizes: Record<string, number>;
  complete: boolean;
}

export type PreviewKind = "text" | "image" | "svg" | "video" | "audio" | "pdf" | "binary";

export interface FileMeta {
  kind: "file" | "dir" | "other";
  size: number;
  mode: number;
  modified: number;
  preview: PreviewKind | null;
}

/** A time window in unix seconds. */
export interface TimeWindow {
  from: number;
  to: number;
}

export interface HostPoint {
  t: number;
  cpu: number;
  mem: number;
  mem_total: number;
  disk: number;
  disk_total: number;
  load1: number | null;
}

/** The domain kuroe's own UI answers on. */
export interface UiDomain {
  hostname: string;
  tls: boolean;
}

export interface ApiToken {
  id: string;
  user_id: string;
  username: string;
  name: string;
  created_at: number;
  expires_at: number | null;
  last_used_at: number | null;
}

export interface NetworkPoint {
  t: number;
  rx_rate: number;
  tx_rate: number;
}

export interface ServicePoint {
  t: number;
  cpu: number;
  mem: number;
  mem_limit: number;
  rx_rate: number;
  tx_rate: number;
  read_rate: number;
  write_rate: number;
}

export interface StorageSnapshot {
  ts: number | null;
  categories: Record<string, number>;
}

export interface LogFile {
  service: string;
  date: string;
  bytes: number;
}

export type InstallMode =
  | { mode: "container"; id: string; name: string; image: string }
  | { mode: "binary" }
  | { mode: "unsupported" };

export interface VersionInfo {
  version: string;
  api: number;
  build: string | null;
  install: InstallMode;
}

export interface LatestRelease {
  version: string;
  newer: boolean;
  notes: string;
  url: string;
  image: string;
  binary_url: string | null;
  changes: string;
  changes_error: string | null;
}

export interface UpdaterStatus {
  updater: string | null;
  state?: string;
  exit_code?: number | null;
  created?: number;
  log?: string;
}

export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

export const API_BASE = "/api/v1";

const STATUS_NAMES: Record<number, string> = {
  400: "Bad Request",
  401: "Unauthorized",
  403: "Forbidden",
  404: "Not Found",
  408: "Request Timeout",
  413: "Content Too Large",
  429: "Too Many Requests",
  500: "Internal Server Error",
  502: "Bad Gateway",
  503: "Service Unavailable",
  504: "Gateway Timeout",
};

/** HTTP/2 has no status text, and proxies in front of kuroe answer in plain text or HTML, not kuroe's JSON. */
export function failureText(status: number, body: string): string {
  try {
    const error = JSON.parse(body)?.error;
    if (typeof error === "string" && error.trim()) return error;
  } catch {}
  const text = body.trim();
  if (text && text.length <= 300 && !text.startsWith("<")) return text;
  const name = STATUS_NAMES[status];
  return `The request failed with status ${status}${name ? ` ${name}` : ""}.`;
}

async function failure(res: Response): Promise<ApiError> {
  return new ApiError(res.status, failureText(res.status, await res.text().catch(() => "")));
}

async function request<T>(method: string, path: string, body?: unknown, raw?: Blob): Promise<T> {
  const headers: Record<string, string> = {};
  if (method !== "GET") headers["x-kuroe-csrf"] = "1";
  if (body !== undefined) headers["content-type"] = "application/json";
  if (raw) headers["content-type"] = raw.type || "application/octet-stream";
  const res = await fetch(`${API_BASE}${path}`, {
    method,
    headers,
    credentials: "same-origin",
    body: raw ?? (body === undefined ? undefined : JSON.stringify(body)),
  });
  if (!res.ok) throw await failure(res);
  if (res.status === 204 || res.status === 202 || res.status === 201) {
    const text = await res.text();
    return (text ? JSON.parse(text) : undefined) as T;
  }
  return res.json();
}

const enc = encodeURIComponent;

export function socketUrl(path: string): string {
  const scheme = location.protocol === "https:" ? "wss" : "ws";
  return `${scheme}://${location.host}${API_BASE}${path}`;
}

export const api = {
  setupStatus: () => request<{ needed: boolean }>("GET", "/setup"),
  setup: (username: string, password: string, code = "") =>
    request<User>("POST", "/setup", { username, password, code }),
  login: (username: string, password: string) => request<User>("POST", "/login", { username, password }),
  logout: () => request<void>("POST", "/logout"),
  me: () => request<User>("GET", "/me"),
  users: () => request<UserRow[]>("GET", "/users"),
  createUser: (username: string, password: string, role: string) =>
    request<User>("POST", "/users", { username, password, role }),
  updateUser: (id: string, role: string, host_mounts: boolean) =>
    request<void>("PUT", `/users/${id}`, { role, host_mounts }),
  userGrants: (id: string) => request<Grant[]>("GET", `/users/${id}/grants`),
  setUserGrants: (id: string, grants: Grant[]) => request<void>("PUT", `/users/${id}/grants`, grants),
  identities: () => request<Identity[]>("GET", "/identities"),
  createIdentity: (identity: NewIdentity) => request<{ id: string }>("POST", "/identities", identity),
  removeIdentity: (id: string, revoke = false) =>
    request<void>("DELETE", `/identities/${id}${revoke ? "?revoke=true" : ""}`),
  addCertificate: (name: string, certificate: string, key: string) =>
    request<{ id: string; sans: string[]; not_after: number; cloudflare_origin: boolean }>(
      "POST",
      "/identities/certificates",
      {
        name,
        certificate,
        key,
      },
    ),
  createOriginCertificate: (name: string, cloudflare: string, hostnames: string[]) =>
    request<{ id: string; sans: string[]; not_after: number }>("POST", "/identities/certificates/cloudflare", {
      name,
      cloudflare,
      hostnames,
    }),
  setOriginPulls: (id: string, on: boolean, cloudflare: string | null) =>
    request<void>("PUT", `/identities/${id}/origin-pulls`, { on, cloudflare }),
  pinCertificate: (domain: string, certificate: string | null) =>
    request<void>("PUT", `/domains/${domain}/certificate`, { certificate }),
  proxyAttention: () => request<Attention[]>("GET", "/proxy/attention"),
  testIdentity: (id: string) =>
    request<{ message: string; expires_at: number | null }>("POST", `/identities/${id}/test`),
  identityRepos: (id: string) => request<Repo[]>("GET", `/identities/${id}/repos`),
  startGithubApp: (name: string, owner: string) =>
    request<{ action: string; manifest: string }>("POST", "/identities/github/start", { name, owner }),
  roles: () => request<RoleDef[]>("GET", "/roles"),
  createRole: (role: RoleInput) => request<void>("POST", "/roles", role),
  updateRole: (name: string, role: RoleInput) => request<void>("PUT", `/roles/${enc(name)}`, role),
  removeRole: (name: string) => request<void>("DELETE", `/roles/${enc(name)}`),
  deploymentAccess: (id: string) => request<Reach[]>("GET", `/deployments/${id}/access`),
  setGrant: (deployment: string, user: string, role: string) =>
    request<void>("PUT", `/deployments/${deployment}/grants/${user}`, { role }),
  removeGrant: (deployment: string, user: string) =>
    request<void>("DELETE", `/deployments/${deployment}/grants/${user}`),
  setUserPassword: (id: string, password: string) => request<void>("POST", `/users/${id}/password`, { password }),
  removeUser: (id: string) => request<void>("DELETE", `/users/${id}`),
  changePassword: (current: string, password: string) => request<void>("POST", "/me/password", { current, password }),
  dockerInfo: () => request<DaemonInfo>("GET", "/docker/info"),
  pool: () => request<PoolUsage>("GET", "/pool"),
  settings: () => request<Record<string, string>>("GET", "/settings"),
  saveSettings: (values: Record<string, string>) => request<void>("PUT", "/settings", values),
  secretKey: () => request<{ kind: "file" | "data_dir"; path: string }>("GET", "/settings/secret-key"),

  deployments: () => request<DeploymentRow[]>("GET", "/deployments"),
  deployment: (id: string) => request<DeploymentDetail>("GET", `/deployments/${id}`),
  createDeployment: (name: string, spec: Spec) => request<Deployment>("POST", "/deployments", { name, spec }),
  updateDeployment: (id: string, spec: Spec) => request<void>("PUT", `/deployments/${id}`, spec),
  deploy: (id: string) => request<void>("POST", `/deployments/${id}/deploy`),
  cancelDeploy: (id: string) => request<void>("POST", `/deployments/${id}/deploy/cancel`),
  deploySocket: (id: string) => socketUrl(`/deployments/${id}/deploy/ws`),
  sendPreviewRequest: (container: string, outgoing: PreviewRequest) =>
    request<PreviewAnswer>("POST", `/containers/${container}/request`, outgoing),
  deploys: (id: string, offset = 0, limit = 20) =>
    request<{ deploys: DeploySummary[]; more: boolean }>(
      "GET",
      `/deployments/${id}/deploys?offset=${offset}&limit=${limit}`,
    ),
  deployDetail: (id: string, deploy: string) => request<DeployDetail>("GET", `/deployments/${id}/deploys/${deploy}`),
  stop: (id: string) => request<string>("POST", `/deployments/${id}/stop`),
  restart: (id: string) => request<string>("POST", `/deployments/${id}/restart`),
  removeDeployment: (id: string) => request<void>("DELETE", `/deployments/${id}`),
  uploadContext: (id: string, file: Blob) => request<void>("PUT", `/deployments/${id}/context`, undefined, file),
  uploadSite: (id: string, file: Blob, deploy = false) =>
    request<void>("PUT", `/deployments/${id}/site${deploy ? "?deploy=true" : ""}`, undefined, file),

  tokens: (id: string) => request<DeployToken[]>("GET", `/deployments/${id}/tokens`),
  createToken: (id: string, name: string) =>
    request<{ token: DeployToken; secret: string }>("POST", `/deployments/${id}/tokens`, { name }),
  deleteToken: (id: string, token: string) => request<void>("DELETE", `/deployments/${id}/tokens/${token}`),
  apiTokens: (everyone: boolean) => request<ApiToken[]>("GET", everyone ? "/api-tokens" : "/me/api-tokens"),
  createApiToken: (name: string, expires_days: number | null) =>
    request<{ token: ApiToken; value: string }>("POST", "/me/api-tokens", { name, expires_days }),
  revokeApiToken: (id: string, everyone: boolean) =>
    request<void>("DELETE", everyone ? `/api-tokens/${id}` : `/me/api-tokens/${id}`),
  ciConfig: (id: string, platform: CiPlatform, runOn: "hosted" | "here") =>
    request<Pipeline>("GET", `/deployments/${id}/ci-config?platform=${platform}&run_on=${runOn}`),
  webhook: (id: string) => request<WebhookInfo>("GET", `/deployments/${id}/webhook`),
  rotateWebhook: (id: string) => request<{ urls: HookUrls; secret: string }>("POST", `/deployments/${id}/webhook`),
  installWebhook: (id: string) => request<{ installed: boolean }>("POST", `/deployments/${id}/webhook/install`),
  disableWebhook: (id: string) => request<void>("DELETE", `/deployments/${id}/webhook`),
  savedSecrets: () => request<SavedSecret[]>("GET", "/saved-secrets"),
  createSavedSecret: (body: { name: string; description: string; value: string }) =>
    request<SavedSecret>("POST", "/saved-secrets", body),
  updateSavedSecret: (id: string, body: { name: string; description: string; value: string }) =>
    request<void>("PUT", `/saved-secrets/${id}`, body),
  deleteSavedSecret: (id: string) => request<void>("DELETE", `/saved-secrets/${id}`),
  revealSavedSecret: (id: string) => request<{ value: string }>("POST", `/saved-secrets/${id}/reveal`),
  matchSavedSecret: (value: string) =>
    request<{ id: string; name: string } | null>("POST", "/saved-secrets/match", { value }),
  exportUrl: (id: string, secrets: boolean) => `${API_BASE}/deployments/${id}/export?secrets=${secrets}`,
  serverExportUrl: (secrets: boolean) => `${API_BASE}/export?secrets=${secrets}`,
  importPreview: (bundle: string) =>
    request<ImportPlan>("POST", "/import/preview", undefined, new Blob([bundle], { type: "text/plain" })),
  importApply: (bundle: string, secrets: Record<string, string>) =>
    request<ImportResult>("POST", "/import/apply", { bundle, secrets }),

  version: () => request<VersionInfo>("GET", "/version"),
  startUpdate: (body: { image?: string; binary_url?: string; sha256?: string }) =>
    request<{ updater?: string; sha256?: string }>("POST", "/update", body),
  updateStatus: () => request<UpdaterStatus>("GET", "/update/status"),
  latestRelease: () => request<LatestRelease>("GET", "/update/latest"),

  hostMetrics: (w: TimeWindow) =>
    request<{ bucket: number; points: HostPoint[]; network: NetworkPoint[] }>(
      "GET",
      `/metrics/host?from=${w.from}&to=${w.to}`,
    ),
  deploymentMetrics: (id: string, w: TimeWindow) =>
    request<{
      bucket: number;
      services: Record<string, ServicePoint[]>;
      containers: { service: string; container_id: string; points: ServicePoint[] }[];
    }>("GET", `/deployments/${id}/metrics?from=${w.from}&to=${w.to}`),
  metricsByDeployment: (w: TimeWindow) =>
    request<{ bucket: number; deployments: { id: string; name: string; points: ServicePoint[] }[] }>(
      "GET",
      `/metrics/deployments?from=${w.from}&to=${w.to}`,
    ),
  storage: () => request<StorageSnapshot>("GET", "/storage"),
  refreshStorage: () => request<StorageSnapshot>("POST", "/storage/refresh"),
  logFiles: (id: string) => request<LogFile[]>("GET", `/deployments/${id}/logs`),
  logSocket: (id: string, service: string, date: string) =>
    socketUrl(`/deployments/${id}/logs/ws?service=${enc(service)}&date=${enc(date)}`),

  proxy: () =>
    request<{
      settings: ProxySettings;
      traefik_running: boolean;
      web_server_error: string | null;
      ui: UiDomain | null;
    }>("GET", "/proxy"),
  saveUiDomain: (ui: UiDomain) => request<{ ui: UiDomain | null; public_url: string }>("PUT", "/proxy/ui", ui),
  checkUiDomain: () => request<{ ok: boolean; message: string }>("POST", "/proxy/ui/check"),
  saveProxy: (settings: ProxySettings) => request<void>("PUT", "/proxy", settings),
  proxyPorts: () => request<{ holders: PortHolder[]; message: string | null }>("GET", "/proxy/ports"),
  previewWebServer: (settings: ProxySettings) =>
    request<WebServerPreview>("POST", "/proxy/webserver/preview", settings),
  installWebServer: () => request<{ output: string }>("POST", "/proxy/webserver/install"),
  domains: (deployment: string) => request<Domain[]>("GET", `/deployments/${deployment}/domains`),
  addDomain: (
    deployment: string,
    hostname: string,
    service: string,
    port: number,
    tls: boolean,
    certificate: string | null = null,
    options: RouteOptions = {},
  ) =>
    request<Domain>("POST", `/deployments/${deployment}/domains`, {
      hostname,
      service,
      port,
      tls,
      certificate,
      options,
    }),
  updateDomain: (id: string, service: string, port: number, options: RouteOptions) =>
    request<Domain>("PUT", `/domains/${id}`, { service, port, options }),
  removeDomain: (id: string) => request<void>("DELETE", `/domains/${id}`),
  snippets: (id: string) => request<Snippet[]>("GET", `/domains/${id}/snippets`),
  verifyDomain: (id: string) => request<{ ok: boolean; message: string }>("POST", `/domains/${id}/verify`),

  runners: () => request<RunnerPool[]>("GET", "/runners"),
  createRunners: (input: RunnerInput, confirmPublic = false) =>
    request<RunnerPool>("POST", "/runners", { ...input, confirm_public: confirmPublic }),
  updateRunners: (id: string, input: RunnerInput) => request<void>("PUT", `/runners/${id}`, input),
  removeRunners: (id: string) => request<{ leftovers: string[] }>("DELETE", `/runners/${id}`),
  clearRunnerCache: (id: string) => request<{ removed: number }>("POST", `/runners/${id}/clear-cache`),
  runnerLogs: async (id: string, instance: string): Promise<string> => {
    const res = await fetch(`${API_BASE}/runners/${id}/instances/${enc(instance)}/logs`, {
      credentials: "same-origin",
    });
    if (!res.ok) throw await failure(res);
    return res.text();
  },

  volumes: () => request<Volume[]>("GET", "/volumes"),
  createVolume: (name: string) => request<void>("POST", "/volumes", { name }),
  removeVolume: (name: string) => request<void>("DELETE", `/volumes/${enc(name)}`),
  deploymentVolumes: () => request<VolumeInfo[]>("GET", "/deployment-volumes"),
  volumesOf: (id: string) => request<VolumeInfo[]>("GET", `/deployments/${id}/volumes`),
  setVolumeLimit: (name: string, limit: VolumeLimit) => request<void>("PUT", `/volume-limits/${enc(name)}`, limit),
  removeVolumeLimit: (name: string) => request<void>("DELETE", `/volume-limits/${enc(name)}`),

  createExec: (container: string, cmd?: string[]) =>
    request<{ id: string }>("POST", `/containers/${container}/exec`, { cmd }),
  resizeExec: (exec: string, cols: number, rows: number) =>
    request<void>("POST", `/exec/${exec}/resize`, { cols, rows }),
  resizeContainer: (container: string, cols: number, rows: number) =>
    request<void>("POST", `/containers/${container}/resize`, { cols, rows }),
  files: (container: string, path: string) =>
    request<Listing>("GET", `/containers/${container}/files?path=${enc(path)}`),
  folderSizes: (container: string, path: string) =>
    request<FolderSizes>("GET", `/containers/${container}/files/sizes?path=${enc(path)}`),
  fileMeta: (container: string, path: string) =>
    request<FileMeta>("GET", `/containers/${container}/files/preview?meta=true&path=${enc(path)}`),
  previewUrl: (container: string, path: string, max?: number) =>
    `${API_BASE}/containers/${container}/files/preview?path=${enc(path)}${max ? `&max=${max}` : ""}`,
  mediaUrl: (container: string, path: string) => `${API_BASE}/containers/${container}/files/media?path=${enc(path)}`,
  dropMedia: (container: string, path: string) =>
    request<void>("DELETE", `/containers/${container}/files/media?path=${enc(path)}`),
  previewText: async (container: string, path: string, max: number): Promise<string> => {
    const res = await fetch(api.previewUrl(container, path, max), { credentials: "same-origin" });
    if (!res.ok) throw await failure(res);
    return res.text();
  },
  uploadFile: (container: string, dir: string, file: File) =>
    request<void>("PUT", `/containers/${container}/files?dir=${enc(dir)}&name=${enc(file.name)}`, undefined, file),
  downloadUrl: (container: string, path: string) =>
    `${API_BASE}/containers/${container}/files/download?path=${enc(path)}`,
};
