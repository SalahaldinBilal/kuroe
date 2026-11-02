import { createEffect, createMemo, createResource, createSignal, Show } from "solid-js";
import { useUser } from "../App";
import { api, can, isOwner } from "../api";
import { ApiTokens } from "../components/ApiTokens";
import { EXPORT_HINT } from "../components/CiPanel";
import { HttpOffConfirm, onHttps } from "../components/HttpAccess";
import { windowOf } from "../components/RangePicker";
import { SelfUpdate } from "../components/SelfUpdate";
import { Card, Note, PageHeader } from "../components/ui/Blocks";
import Button from "../components/ui/Button/Button";
import EmptyState from "../components/ui/EmptyState/EmptyState";
import { Slider } from "../components/ui/Form";
import { Icon } from "../components/ui/Icon";
import Input, { Unit } from "../components/ui/Input/Input";
import { ready, SaveBar, same } from "../components/ui/Overlay";
import Select from "../components/ui/Select/Select";
import SettingRow from "../components/ui/SettingRow/SettingRow";
import { Bone, Busy, SkeletonCardHead, SkeletonSettings } from "../components/ui/Skeleton";
import useToastState from "../states/toastState";

type Values = Record<string, string>;

const KEYS = [
  "public_url",
  "pool_cpus",
  "pool_memory_mb",
  "pool_storage_gb",
  "default_cpus",
  "default_memory_mb",
  "default_pids",
  "default_net_in_mbit",
  "default_net_out_mbit",
  "default_net_conn_out_mbit",
  "retention_metrics_days",
  "retention_logs_days",
  "http_access",
  "trusted_proxies",
] as const;

const TEXT_KEYS = new Set(["public_url", "http_access", "trusted_proxies"]);

const LABELS: Record<string, string> = {
  public_url: "Public URL",
  pool_cpus: "CPU pool",
  pool_memory_mb: "Memory pool",
  pool_storage_gb: "Storage pool",
  default_cpus: "Default CPUs",
  default_memory_mb: "Default memory",
  default_pids: "Default process limit",
  default_net_in_mbit: "Default download limit",
  default_net_out_mbit: "Default upload limit",
  default_net_conn_out_mbit: "Default upload per connection",
  retention_metrics_days: "Metrics retention",
  retention_logs_days: "Logs retention",
};

export function Settings() {
  const user = useUser();
  const { pushToast, pushError } = useToastState;
  const [values, { refetch }] = createResource(api.settings);
  const [info] = createResource(api.dockerInfo);
  const [host] = createResource(() => windowOf({ preset: "1h" }), api.hostMetrics);
  const [secretKey] = createResource(() => can(user(), "settings") || undefined, api.secretKey);
  const baseline = createMemo(() => {
    const v = ready(values);
    return v ? (Object.fromEntries(KEYS.map((k) => [k, v[k] ?? ""])) as Values) : null;
  });
  const [draft, setDraft] = createSignal<Values | null>(null);
  const [busy, setBusy] = createSignal(false);
  const canWrite = () => can(user(), "settings");
  const dirty = () => draft() !== null && !same(draft(), baseline());

  createEffect(() => {
    const b = baseline();
    if (b && !draft()) setDraft(b);
  });

  const hostCpus = () => ready(info)?.cpus ?? 8;
  const hostMemoryMb = () => Math.floor((ready(info)?.memory_bytes ?? 8 * 2 ** 30) / 2 ** 20);
  const diskGb = () => {
    const total = host.latest?.points.at(-1)?.disk_total;
    return total ? Math.floor(total / 1e9) : 1000;
  };

  const text = (key: string) => draft()?.[key] ?? "";
  const num = (key: string) => (text(key).trim() ? Number(text(key)) : null);

  function set(key: string, value: string) {
    const d = draft();
    if (!d) return;
    setDraft({ ...d, [key]: value });
  }

  const setNum = (key: string) => (v: number | null) => set(key, v == null ? "" : String(v));

  const httpAccess = () => text("http_access") || "open";
  const [confirmingHttp, setConfirmingHttp] = createSignal(false);

  function save() {
    const turningOff = httpAccess() === "local" && (baseline()?.http_access || "open") !== "local";
    if (turningOff && !confirmingHttp()) return setConfirmingHttp(true);
    return store();
  }

  async function store() {
    const d = draft();
    if (!d) return;
    try {
      const next: Values = {};
      for (const key of KEYS) {
        const value = (d[key] ?? "").trim();
        if (key === "public_url") {
          if (value && !/^https?:\/\/[^\s/]+/.test(value))
            throw new Error("Public URL must start with http:// or https://");
        } else if (!TEXT_KEYS.has(key) && value && !(Number(value) > 0)) {
          throw new Error(`${LABELS[key]} must be a positive number`);
        }
        next[key] = value;
      }
      setBusy(true);
      await api.saveSettings(next);
      pushToast("Settings saved");
      await refetch();
      setDraft(baseline());
    } catch (err) {
      pushError(err);
    } finally {
      setBusy(false);
      setConfirmingHttp(false);
    }
  }

  return (
    <div class="content narrow">
      <PageHeader title="Settings" description="Empty fields use the default shown in grey" />
      <div class="stack">
        <Show
          when={draft()}
          fallback={
            <Show
              when={values.error}
              fallback={
                <Busy>
                  <SkeletonSettings lines={[1, 2, 2]} />
                  <SkeletonSettings lines={[1, 4, 1]} description />
                  <SkeletonSettings lines={[0, 0, 0]} description />
                  <SkeletonSettings lines={[2, 2]} />
                </Busy>
              }
            >
              <Card>
                <EmptyState
                  icon="alert"
                  tone="danger"
                  title="Couldn't load settings"
                  description={String(values.error?.message)}
                />
              </Card>
            </Show>
          }
        >
          <Card title="General">
            <SettingRow label="Public URL" description="Used in webhook and CI links.">
              <Input
                mono
                label="Public URL"
                disabled={!canWrite()}
                placeholder="https://kuroe.example.com"
                value={text("public_url")}
                onValue={(v) => set("public_url", v)}
              />
            </SettingRow>
            <SettingRow
              label="Plain HTTP access"
              description={
                !onHttps() && httpAccess() === "open"
                  ? "Open this page over HTTPS to turn plain HTTP off, so you can't lock yourself out."
                  : "Who may use kuroe without HTTPS. This server itself always can."
              }
            >
              <Select
                label="Plain HTTP access"
                disabled={!canWrite() || (!onHttps() && httpAccess() === "open")}
                items={[
                  { value: "open", label: "Open" },
                  { value: "local", label: "Only from this server and HTTPS proxies" },
                ]}
                value={httpAccess()}
                onChange={(v) => set("http_access", v)}
              />
            </SettingRow>
            <Show when={httpAccess() === "local"}>
              <SettingRow
                label="Trusted proxies"
                description="HTTPS proxies outside this server, by address or range. Managed Traefik is trusted already."
              >
                <Input
                  mono
                  label="Trusted proxies"
                  disabled={!canWrite()}
                  placeholder="10.0.0.5, 192.168.1.0/24"
                  value={text("trusted_proxies")}
                  onValue={(v) => set("trusted_proxies", v)}
                />
              </SettingRow>
            </Show>
            <Show when={isOwner(user())}>
              <SettingRow label="Setup" description="Default limits, web traffic and accounts, step by step.">
                <Button size="sm" href="/onboarding">
                  Run setup again
                </Button>
              </SettingRow>
            </Show>
          </Card>
          <Card
            title="Resource pool"
            description="What running containers may use together. New deploys are checked against real use."
          >
            <SettingRow label="CPU" description="Default is all host CPUs.">
              <Slider
                label="CPU pool"
                disabled={!canWrite()}
                value={num("pool_cpus")}
                onChange={setNum("pool_cpus")}
                min={0.5}
                max={hostCpus()}
                step={0.5}
                unit="cores"
                fallback={hostCpus()}
              />
            </SettingRow>
            <SettingRow
              label="Memory"
              description="Default is all host memory. Containers that grow at the same time can still run the host out of memory. Memory limits on each deployment prevent that."
            >
              <Slider
                label="Memory pool"
                disabled={!canWrite()}
                value={num("pool_memory_mb")}
                onChange={setNum("pool_memory_mb")}
                min={256}
                max={hostMemoryMb()}
                step={256}
                unit="MB"
                fallback={hostMemoryMb()}
              />
            </SettingRow>
            <SettingRow label="Storage" description="Default is no limit.">
              <Slider
                label="Storage pool"
                disabled={!canWrite()}
                value={num("pool_storage_gb")}
                onChange={setNum("pool_storage_gb")}
                min={1}
                max={diskGb()}
                step={1}
                unit="GB"
                fallback={null}
              />
            </SettingRow>
          </Card>
          <Card
            title="Container defaults"
            description="Used when a service doesn't set its own limit. Clear it or drag all the way right for no limit."
          >
            <SettingRow label="CPUs">
              <Slider
                label="Default CPUs"
                disabled={!canWrite()}
                value={num("default_cpus")}
                onChange={setNum("default_cpus")}
                min={0.25}
                max={hostCpus()}
                step={0.25}
                unit="cores"
                fallback={null}
              />
            </SettingRow>
            <SettingRow label="Memory">
              <Slider
                label="Default memory"
                disabled={!canWrite()}
                value={num("default_memory_mb")}
                onChange={setNum("default_memory_mb")}
                min={64}
                max={hostMemoryMb()}
                step={64}
                unit="MB"
                fallback={null}
              />
            </SettingRow>
            <SettingRow label="Process limit">
              <Slider
                label="Default process limit"
                disabled={!canWrite()}
                value={num("default_pids")}
                onChange={setNum("default_pids")}
                min={64}
                max={4096}
                step={64}
                unit="pids"
                fallback={null}
              />
            </SettingRow>
            <SettingRow label="Download limit" description="Per container. Counts traffic from the proxy too.">
              <Input
                mono
                inputMode="decimal"
                label="Default download limit"
                placeholder="No limit"
                disabled={!canWrite()}
                value={text("default_net_in_mbit")}
                onValue={(v) => set("default_net_in_mbit", v)}
                after={<Unit>Mbit/s</Unit>}
              />
            </SettingRow>
            <SettingRow label="Upload limit" description="Per container. Not applied in Swarm mode.">
              <Input
                mono
                inputMode="decimal"
                label="Default upload limit"
                placeholder="No limit"
                disabled={!canWrite()}
                value={text("default_net_out_mbit")}
                onValue={(v) => set("default_net_out_mbit", v)}
                after={<Unit>Mbit/s</Unit>}
              />
            </SettingRow>
            <SettingRow
              label="Upload per connection"
              description="Caps each connection on its own, under the upload limit."
            >
              <Input
                mono
                inputMode="decimal"
                label="Default upload per connection"
                placeholder="No limit"
                disabled={!canWrite()}
                value={text("default_net_conn_out_mbit")}
                onValue={(v) => set("default_net_conn_out_mbit", v)}
                after={<Unit>Mbit/s</Unit>}
              />
            </SettingRow>
          </Card>
          <Card title="Retention">
            <SettingRow label="Metrics" description="Samples older than this many days are deleted. Default is 30.">
              <Input
                mono
                inputMode="numeric"
                label="Metrics retention"
                placeholder="30"
                disabled={!canWrite()}
                value={text("retention_metrics_days")}
                onValue={(v) => set("retention_metrics_days", v)}
                after={<Unit>days</Unit>}
              />
            </SettingRow>
            <SettingRow label="Logs" description="Log files older than this many days are deleted. Default is 30.">
              <Input
                mono
                inputMode="numeric"
                label="Logs retention"
                placeholder="30"
                disabled={!canWrite()}
                value={text("retention_logs_days")}
                onValue={(v) => set("retention_logs_days", v)}
                after={<Unit>days</Unit>}
              />
            </SettingRow>
          </Card>
          <SaveBar
            show={dirty() && canWrite()}
            busy={busy()}
            message="Unsaved changes."
            onSave={save}
            onDiscard={() => setDraft(baseline())}
          />
          <HttpOffConfirm
            show={confirmingHttp()}
            busy={busy()}
            onConfirm={store}
            onHide={() => setConfirmingHttp(false)}
          />
        </Show>

        <Show
          when={canWrite() && ready(secretKey)}
          fallback={
            <Show when={canWrite() && !secretKey.latest && !secretKey.error}>
              <Busy>
                <section class="card">
                  <SkeletonCardHead description />
                  <div class="card-b stack" style={{ gap: "10px" }}>
                    <Bone w="70%" />
                    <Bone w="85%" size="sm" />
                  </div>
                </section>
              </Busy>
            </Show>
          }
        >
          {(key) => (
            <Card title="Secrets" description="Saved tokens and secret values are encrypted with this key">
              <div class="card-b stack" style={{ gap: "10px" }}>
                <div>
                  {key().kind === "file" ? "Key file " : "Inside the data folder at "}
                  <span class="mono">{key().path}</span>
                </div>
                <Show when={key().kind === "data_dir"}>
                  <Note tone="warn">
                    A copy of the data folder includes the key. Point KUROE_SECRET_KEY_FILE at a file outside it.
                  </Note>
                </Show>
                <span class="hint">
                  Without this key, saved tokens and secret values can't be read and have to be entered again.
                </span>
              </div>
            </Card>
          )}
        </Show>

        <ApiTokens owner={isOwner(user())} publicUrl={ready(values)?.public_url ?? ""} />

        <Show when={isOwner(user())}>
          <SelfUpdate />
          <Card
            title="Export this server"
            description="Every deployment, domain, shared volume, setting and user role. Password hashes are never included."
          >
            <div class="card-b hrow wrap">
              <Button download href={api.serverExportUrl(false)}>
                <Icon name="download" />
                Download
              </Button>
              <Button download href={api.serverExportUrl(true)}>
                <Icon name="lock" />
                Download with secrets
              </Button>
              <span class="hint">{EXPORT_HINT}</span>
            </div>
          </Card>
        </Show>
      </div>
    </div>
  );
}
