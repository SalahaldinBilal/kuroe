import { createMemo, createResource, createSignal, For, Show, untrack } from "solid-js";
import { api, type DeploymentDetail, type ServiceSettings, type Spec } from "../api";
import { Card, Note } from "../components/ui/Blocks";
import Button from "../components/ui/Button/Button";
import { EnvRows, MountRows, PortRows } from "../components/ui/Editors";
import Field from "../components/ui/Field/Field";
import { Slider } from "../components/ui/Form";
import { Icon } from "../components/ui/Icon";
import Input, { Unit } from "../components/ui/Input/Input";
import { ready, SaveBar, same } from "../components/ui/Overlay";
import SettingRow from "../components/ui/SettingRow/SettingRow";
import Switch from "../components/ui/Switch/Switch";
import {
  type EnvRow,
  envToRows,
  isCompose,
  type MountRow,
  mbitText,
  mountsToRows,
  type PortRow,
  parseMbit,
  portsToRows,
  rowsToEnv,
  rowsToMounts,
  rowsToPorts,
  withEnvRefs,
} from "../spec";
import useToastState, { errorMessage } from "../states/toastState";

export const HOST_ACCESS_HINT =
  "Containers reach this server at host.docker.internal. Services on the server must listen on 0.0.0.0 or the Docker bridge address, not only 127.0.0.1.";

interface Props {
  detail: DeploymentDetail;
  onSaved: (spec: Spec) => void;
}

interface ServiceDraft {
  public: boolean;
  cpus: number | null;
  memory_mb: number | null;
  pids: number | null;
  net_in: string;
  net_out: string;
  net_conn_out: string;
  mounts: MountRow[];
}

interface Draft {
  ports: PortRow[];
  env: EnvRow[];
  services: Record<string, ServiceDraft>;
  hostAccess: boolean;
}

function serviceNames(d: DeploymentDetail): string[] {
  const names = new Set(Object.keys(d.spec.services));
  for (const c of d.containers) if (c.service) names.add(c.service);
  if (!isCompose(d.spec)) names.add("app");
  return [...names].sort();
}

function serviceDraft(s: ServiceSettings | undefined): ServiceDraft {
  return {
    public: s?.public ?? false,
    cpus: s?.cpus ?? null,
    memory_mb: s?.memory_mb ?? null,
    pids: s?.pids ?? null,
    net_in: mbitText(s?.net_in_mbit),
    net_out: mbitText(s?.net_out_mbit),
    net_conn_out: mbitText(s?.net_conn_out_mbit),
    mounts: mountsToRows(s?.volumes ?? []),
  };
}

function draftOf(d: DeploymentDetail): Draft {
  return {
    ports: portsToRows(d.spec.container.ports),
    env: envToRows(d.spec.container.env, d.spec.secrets ?? [], d.spec.secret_refs),
    services: Object.fromEntries(serviceNames(d).map((n) => [n, serviceDraft(d.spec.services[n])])),
    hostAccess: d.spec.host_access ?? false,
  };
}

export function ServiceSettingsForm(props: Props) {
  const { pushToast } = useToastState;
  const [volumes] = createResource(api.volumes);
  const [info] = createResource(api.dockerInfo);
  const [settings] = createResource(api.settings);
  const baseline = createMemo(() => draftOf(props.detail));
  const [draft, setDraft] = createSignal<Draft>(untrack(baseline));
  const [newService, setNewService] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");

  const dirty = () => !same(draft(), baseline());
  const single = () => !isCompose(props.detail.spec);
  const names = () => Object.keys(draft().services).sort();
  const volumeNames = () => (ready(volumes) ?? []).map((v) => v.name);

  const fallback = (key: "default_cpus" | "default_memory_mb" | "default_pids") => {
    const value = Number(ready(settings)?.[key]);
    return value > 0 ? value : null;
  };
  const netPlaceholder = (key: "default_net_in_mbit" | "default_net_out_mbit" | "default_net_conn_out_mbit") => {
    const value = Number(ready(settings)?.[key]);
    return value > 0 ? `Default ${value}` : "No limit";
  };
  const stack = () => props.detail.spec.mode === "stack";
  const hostCpus = () => ready(info)?.cpus ?? 8;
  const hostMemoryMb = () => Math.floor((ready(info)?.memory_bytes ?? 8 * 2 ** 30) / 2 ** 20);

  function discard() {
    setDraft(baseline());
    setError("");
  }

  function patchService(name: string, change: Partial<ServiceDraft>) {
    const d = draft();
    setDraft({ ...d, services: { ...d.services, [name]: { ...d.services[name], ...change } } });
  }

  function addService() {
    const name = newService().trim();
    const d = draft();
    if (!name || d.services[name]) return;
    setDraft({ ...d, services: { ...d.services, [name]: serviceDraft(undefined) } });
    setNewService("");
  }

  async function save() {
    setError("");
    setBusy(true);
    try {
      const d = draft();
      const nextServices: Record<string, ServiceSettings> = {};
      for (const [name, s] of Object.entries(d.services)) {
        nextServices[name] = {
          public: s.public,
          cpus: s.cpus,
          memory_mb: s.memory_mb,
          pids: s.pids,
          net_in_mbit: parseMbit(s.net_in, `${name}: download limit`),
          net_out_mbit: parseMbit(s.net_out, `${name}: upload limit`),
          net_conn_out_mbit: parseMbit(s.net_conn_out, `${name}: per-connection upload limit`),
          volumes: rowsToMounts(s.mounts),
        };
      }
      const current = props.detail.spec;
      const { env: envMap, secrets, refs } = rowsToEnv(d.env);
      const spec: Spec = {
        ...current,
        services: nextServices,
        container: {
          ...current.container,
          ports: single() ? rowsToPorts(d.ports) : current.container.ports,
          env: envMap,
        },
        secrets,
        secret_refs: withEnvRefs(current, refs),
        host_access: d.hostAccess,
      };
      await api.updateDeployment(props.detail.id, spec);
      props.onSaved(spec);
      setDraft(untrack(baseline));
      pushToast("Saved. Deploy to apply the changes.");
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div class="stack">
      <Show when={single()}>
        <Card title="Container">
          <div class="card-b stack" style={{ gap: "20px" }}>
            <Field label="Ports" hint="Host ports bind to 127.0.0.1 unless the service is public.">
              <PortRows rows={draft().ports} onChange={(ports) => setDraft({ ...draft(), ports })} />
            </Field>
            <Field label="Environment" hint="Locked values are secret. Hidden from viewers and left out of exports.">
              <EnvRows
                rows={draft().env}
                names={props.detail.secret_names}
                onChange={(env) => setDraft({ ...draft(), env })}
              />
            </Field>
          </div>
        </Card>
      </Show>

      <Show when={!single()}>
        <Card title="Variables" description="They fill ${KEY} in the Compose file when it deploys">
          <div class="card-b">
            <Field label="Environment" hint="Locked values are secret. Hidden from viewers and left out of exports.">
              <EnvRows
                rows={draft().env}
                names={props.detail.secret_names}
                onChange={(env) => setDraft({ ...draft(), env })}
              />
            </Field>
          </div>
        </Card>
      </Show>

      <Card title="Network">
        <SettingRow label="Allow access to this machine" description={HOST_ACCESS_HINT}>
          <Switch checked={draft().hostAccess} onChange={(hostAccess) => setDraft({ ...draft(), hostAccess })} />
        </SettingRow>
      </Card>

      <For each={names()}>
        {(name) => {
          const s = () => draft().services[name];
          return (
            <Show when={s()}>
              <Card
                title={
                  single() ? (
                    "Limits and volumes"
                  ) : (
                    <>
                      Service <span class="mono">{name}</span>
                    </>
                  )
                }
                description="Empty limits use the server default from Settings"
              >
                <SettingRow label="Public ports" description="Bind ports on all interfaces instead of 127.0.0.1.">
                  <Switch checked={s().public} onChange={(v) => patchService(name, { public: v })} />
                </SettingRow>
                <SettingRow label="CPUs">
                  <Slider
                    label={`${name} CPUs`}
                    value={s().cpus}
                    onChange={(cpus) => patchService(name, { cpus })}
                    min={0.25}
                    max={hostCpus()}
                    step={0.25}
                    unit="cores"
                    fallback={fallback("default_cpus")}
                  />
                </SettingRow>
                <SettingRow label="Memory">
                  <Slider
                    label={`${name} memory`}
                    value={s().memory_mb}
                    onChange={(memory_mb) => patchService(name, { memory_mb })}
                    min={64}
                    max={hostMemoryMb()}
                    step={64}
                    unit="MB"
                    fallback={fallback("default_memory_mb")}
                  />
                </SettingRow>
                <SettingRow label="Process limit">
                  <Slider
                    label={`${name} process limit`}
                    value={s().pids}
                    onChange={(pids) => patchService(name, { pids })}
                    min={64}
                    max={4096}
                    step={64}
                    unit="pids"
                    fallback={fallback("default_pids")}
                  />
                </SettingRow>
                <SettingRow
                  label="Download limit"
                  description={
                    stack()
                      ? "Not available in Swarm mode yet."
                      : "Counts traffic from the proxy and other containers too."
                  }
                >
                  <Input
                    mono
                    inputMode="decimal"
                    label={`${name} download limit`}
                    disabled={stack()}
                    placeholder={netPlaceholder("default_net_in_mbit")}
                    value={s().net_in}
                    onValue={(net_in) => patchService(name, { net_in })}
                    after={<Unit>Mbit/s</Unit>}
                  />
                </SettingRow>
                <SettingRow
                  label="Upload limit"
                  description={stack() ? "Not available in Swarm mode yet." : "Applies to each container."}
                >
                  <Input
                    mono
                    inputMode="decimal"
                    label={`${name} upload limit`}
                    disabled={stack()}
                    placeholder={netPlaceholder("default_net_out_mbit")}
                    value={s().net_out}
                    onValue={(net_out) => patchService(name, { net_out })}
                    after={<Unit>Mbit/s</Unit>}
                  />
                </SettingRow>
                <SettingRow
                  label="Upload per connection"
                  description={
                    stack()
                      ? "Not available in Swarm mode yet."
                      : "Caps each connection on its own. Traffic through the proxy shares a few connections, so this fits direct traffic best."
                  }
                >
                  <Input
                    mono
                    inputMode="decimal"
                    label={`${name} upload per connection`}
                    disabled={stack()}
                    placeholder={netPlaceholder("default_net_conn_out_mbit")}
                    value={s().net_conn_out}
                    onValue={(net_conn_out) => patchService(name, { net_conn_out })}
                    after={<Unit>Mbit/s</Unit>}
                  />
                </SettingRow>
                <SettingRow
                  type="stack"
                  label="Shared volumes"
                  description="Mount a volume from the Volumes page into this service."
                >
                  <Show
                    when={volumeNames().length > 0 || s().mounts.length > 0}
                    fallback={<span class="hint">No shared volumes yet. Create one on the Volumes page.</span>}
                  >
                    <MountRows
                      rows={s().mounts}
                      volumes={volumeNames()}
                      onChange={(mounts) => patchService(name, { mounts })}
                    />
                  </Show>
                </SettingRow>
              </Card>
            </Show>
          );
        }}
      </For>

      <Show when={!single()}>
        <Card>
          <form
            class="card-b hrow"
            onSubmit={(e) => {
              e.preventDefault();
              addService();
            }}
          >
            <Input
              mono
              class="grow"
              label="Service name"
              placeholder="Service name from compose.yaml"
              value={newService()}
              onValue={setNewService}
            />
            <Button type="submit" disabled={!newService().trim()}>
              <Icon name="plus" />
              Add service
            </Button>
          </form>
        </Card>
      </Show>

      <Show when={error()}>
        <Note tone="bad">{error()}</Note>
      </Show>

      <SaveBar
        show={dirty()}
        busy={busy()}
        message="Unsaved changes. Deploy after saving to apply them."
        onSave={save}
        onDiscard={discard}
      />
    </div>
  );
}
