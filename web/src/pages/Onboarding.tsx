import { createEffect, createResource, createSignal, For, Match, Show, Switch } from "solid-js";
import { api, type PortHolder, type WebServerKind } from "../api";
import { AddCertificate } from "../components/AddCertificate";
import { AddIdentity } from "../components/AddIdentity";
import { Card, Note, Pill, Steps } from "../components/ui/Blocks";
import Button from "../components/ui/Button/Button";
import EmptyState, { Loading } from "../components/ui/EmptyState/EmptyState";
import Field from "../components/ui/Field/Field";
import { Slider, Tiles } from "../components/ui/Form";
import { Icon } from "../components/ui/Icon";
import Input from "../components/ui/Input/Input";
import { Logo } from "../components/ui/Logo";
import { ready } from "../components/ui/Overlay";
import Select from "../components/ui/Select/Select";
import SettingRow from "../components/ui/SettingRow/SettingRow";
import Spinner from "../components/ui/Spinner/Spinner";
import { PROVIDERS, type Provider } from "../identities";
import useToastState, { errorMessage } from "../states/toastState";
import { KINDS, serverDefaults, serverOf, WEB_SERVERS } from "../webserver";

export const ONBOARDING_DONE = "onboarding_done";

const LABELS = ["Limits", "Web traffic", "Accounts"];

interface Limits {
  cpus: number | null;
  memory: number | null;
  pids: number | null;
}

type Traffic = "behind" | "write" | "managed" | "off";

const ACCOUNTS: { provider: Provider; kinds: string[]; description: string }[] = [
  {
    provider: "github",
    kinds: ["github_app", "github_token"],
    description: "Private repos, a repo picker and automatic push webhooks.",
  },
  { provider: "gitlab", kinds: ["gitlab"], description: "Private repositories on gitlab.com or your own instance." },
  { provider: "registry", kinds: ["registry"], description: "Pull private images from ghcr.io, Docker Hub or others." },
  {
    provider: "cloudflare",
    kinds: ["cloudflare"],
    description: "HTTPS certificates through DNS, including wildcards.",
  },
];

const optional = (n: number | null) => (n == null ? "" : String(n));

export function Onboarding() {
  const { pushToast, pushError } = useToastState;
  const [step, setStep] = createSignal(0);
  const [busy, setBusy] = createSignal(false);

  const [settings] = createResource(api.settings);
  const [info] = createResource(api.dockerInfo);
  const [ports] = createResource(api.proxyPorts);
  const [proxy] = createResource(api.proxy);
  const [identities, { refetch: refetchIdentities }] = createResource(api.identities);

  const [limited, setLimited] = createSignal<"none" | "set">("none");
  const [limits, setLimits] = createSignal<Limits>({ cpus: 1, memory: 1024, pids: 512 });
  const [traffic, setTraffic] = createSignal<Traffic>("off");
  const [kind, setKind] = createSignal<WebServerKind>("nginx");
  const [email, setEmail] = createSignal("");
  const [adding, setAdding] = createSignal<Provider | null>(null);

  createEffect(() => {
    const s = ready(settings);
    if (!s) return;
    const saved = { cpus: s.default_cpus, memory: s.default_memory_mb, pids: s.default_pids };
    if (saved.cpus || saved.memory || saved.pids) {
      setLimited("set");
      setLimits({
        cpus: saved.cpus ? Number(saved.cpus) : null,
        memory: saved.memory ? Number(saved.memory) : null,
        pids: saved.pids ? Number(saved.pids) : null,
      });
    }
  });

  const holders = () => ready(ports)?.holders ?? [];
  const holder = (): PortHolder | undefined => holders().find((h) => h.by) ?? holders()[0];

  createEffect(() => {
    if (!ready(ports)) return;
    const h = holder();
    if (h?.by) setKind(h.by);
    setTraffic(h ? "behind" : "managed");
  });

  const hostCpus = () => ready(info)?.cpus ?? 8;
  const hostMemoryMb = () => Math.floor((ready(info)?.memory_bytes ?? 8 * 2 ** 30) / 2 ** 20);
  const label = () => WEB_SERVERS[kind()].label;

  function next() {
    setStep(step() + 1);
  }

  async function run(work: () => Promise<void>) {
    setBusy(true);
    try {
      await work();
      next();
    } catch (err) {
      pushError(err);
    } finally {
      setBusy(false);
    }
  }

  function saveLimits() {
    return run(async () => {
      const l = limited() === "set" ? limits() : { cpus: null, memory: null, pids: null };
      await api.saveSettings({
        default_cpus: optional(l.cpus),
        default_memory_mb: optional(l.memory),
        default_pids: optional(l.pids),
      });
    });
  }

  function saveTraffic() {
    return run(async () => {
      const choice = traffic();
      if (choice === "off") return;
      const current = (await api.proxy()).settings;
      if (choice === "managed") {
        await api.saveProxy({ ...current, mode: "managed", bind: "ports", acme_email: email().trim() || null });
        pushToast("Traefik is running");
        return;
      }
      const web_server = serverOf(serverDefaults(kind(), holder()?.container ?? null));
      await api.saveProxy(
        choice === "behind"
          ? { ...current, mode: "managed", bind: "behind", web_server }
          : { ...current, mode: "external", web_server },
      );
      try {
        await api.installWebServer();
        pushToast(`Installed the config into ${label()}`);
      } catch (err) {
        pushToast(
          `Saved, but writing the ${label()} config failed: ${errorMessage(err)}. Fix it on the Proxy page.`,
          "error",
        );
      }
    });
  }

  function finish() {
    return run(() => api.saveSettings({ [ONBOARDING_DONE]: "true" }));
  }

  const connected = (kinds: string[]) => (ready(identities) ?? []).some((i) => kinds.includes(i.kind));

  const trafficHint = (): string => {
    const server = WEB_SERVERS[kind()];
    switch (traffic()) {
      case "behind":
        return `kuroe starts Traefik on ${ready(proxy)?.settings.behind_http ?? "a local address"}, writes ${server.path} and tests ${server.label} before reloading it.`;
      case "write":
        return `kuroe writes ${server.path} for your domains and reloads ${server.label} after each change.`;
      case "managed":
        return "Traefik takes ports 80 and 443 and gets a free certificate from Let's Encrypt for each domain.";
      default:
        return "Nothing changes on this server.";
    }
  };

  function busyText() {
    if (step() !== 1 || traffic() === "off") return "Saving";
    if (traffic() === "write") return `Writing the ${label()} config and reloading it`;
    return "Starting Traefik. The first time it downloads its image, about 250 MB.";
  }

  const footer = (onContinue: () => void, last = false) => (
    <div class="card-f">
      <Show when={step() > 0}>
        <Button variant="ghost" onClick={() => setStep(step() - 1)}>
          <Icon name="back" />
          Back
        </Button>
      </Show>
      <span class="grow hrow onb-busy">
        <Show when={busy()}>
          <Spinner />
          <span class="hint">{busyText()}</span>
        </Show>
      </span>
      <Button variant="ghost" disabled={busy()} onClick={last ? finish : next}>
        Skip
      </Button>
      <Button variant="primary" disabled={busy()} onClick={onContinue}>
        {last ? "Finish" : "Continue"}
      </Button>
    </div>
  );

  return (
    <div class="onb">
      <div class="brand">
        <Logo />
        kuroe
      </div>
      <h1>Set up this server</h1>
      <p class="muted onb-sub">Three quick questions. Every step can be skipped and changed later.</p>
      <Steps labels={LABELS} step={step()} />
      <Switch>
        <Match when={step() === 0}>
          <Card title="Default limits" description="How much a deployment may use when it doesn't set its own limits">
            <div class="card-b">
              <Tiles
                label="Default limits"
                columns={2}
                value={limited()}
                onChange={setLimited}
                options={[
                  {
                    value: "none",
                    label: "No limits",
                    icon: "activity",
                    description: "Deployments can use the whole machine. Fine when it's just you.",
                  },
                  {
                    value: "set",
                    label: "Set limits",
                    icon: "sliders",
                    description: "Cap CPU, memory and processes for each deployment.",
                  },
                ]}
              />
            </div>
            <Show when={limited() === "set"}>
              <div class="onb-rows">
                <SettingRow label="CPUs">
                  <Slider
                    label="Default CPUs"
                    value={limits().cpus}
                    onChange={(cpus) => setLimits({ ...limits(), cpus })}
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
                    value={limits().memory}
                    onChange={(memory) => setLimits({ ...limits(), memory })}
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
                    value={limits().pids}
                    onChange={(pids) => setLimits({ ...limits(), pids })}
                    min={64}
                    max={4096}
                    step={64}
                    unit="pids"
                    fallback={null}
                  />
                </SettingRow>
              </div>
            </Show>
            {footer(saveLimits)}
          </Card>
        </Match>

        <Match when={step() === 1}>
          <Card title="Web traffic" description="How requests for your domains reach deployments">
            <div class="card-b stack">
              <Show when={!ports.loading} fallback={<Loading label="Checking ports 80 and 443" />}>
                <Show
                  when={holder()}
                  fallback={
                    <Tiles
                      label="Web traffic"
                      columns={2}
                      value={traffic()}
                      onChange={setTraffic}
                      options={[
                        {
                          value: "managed",
                          label: "Managed Traefik",
                          icon: "shield",
                          description: "Ports 80 and 443 are free. kuroe runs Traefik on them.",
                        },
                        {
                          value: "off",
                          label: "Not now",
                          icon: "x",
                          description: "Deployments stay on 127.0.0.1. Set this up later under Proxy.",
                        },
                      ]}
                    />
                  }
                >
                  {(h) => (
                    <>
                      <Note icon="server">
                        <b>
                          Found {h().by ? WEB_SERVERS[h().by as WebServerKind].label : "something"} on{" "}
                          {holders().length > 1 ? "ports 80 and 443" : `port ${h().port}`}.
                        </b>{" "}
                        kuroe can work with it instead of taking the ports.
                      </Note>
                      <Show when={!h().by}>
                        <Field label="Which web server is it?">
                          <Select
                            label="Web server"
                            items={KINDS.map((k) => ({ value: k, label: WEB_SERVERS[k].label }))}
                            value={kind()}
                            onChange={setKind}
                          />
                        </Field>
                      </Show>
                      <Tiles
                        label="Web traffic"
                        columns={3}
                        value={traffic()}
                        onChange={setTraffic}
                        options={[
                          {
                            value: "behind",
                            label: `Traefik behind ${label()}`,
                            icon: "shield",
                            description: `${label()} keeps its ports and forwards your kuroe domains. kuroe installs one config file.`,
                          },
                          {
                            value: "write",
                            label: `kuroe writes ${label()} configs`,
                            icon: "code",
                            description: `No Traefik. ${label()} sends each domain to its deployment and reloads after each change.`,
                          },
                          {
                            value: "off",
                            label: "Not now",
                            icon: "x",
                            description: "Deployments stay on 127.0.0.1. Set this up later under Proxy.",
                          },
                        ]}
                      />
                    </>
                  )}
                </Show>
                <Show when={traffic() === "managed"}>
                  <Field label="Email" hint="Let's Encrypt sends notices about your certificates here.">
                    <Input
                      type="email"
                      label="Let's Encrypt email"
                      placeholder="you@example.com"
                      value={email()}
                      onValue={setEmail}
                    />
                  </Field>
                </Show>
                <Show when={trafficHint()} keyed>
                  {(hint) => <span class="hint">{hint}</span>}
                </Show>
              </Show>
            </div>
            {footer(saveTraffic)}
          </Card>
        </Match>

        <Match when={step() === 2}>
          <Card
            title="Git and registries"
            description="Connect accounts once, then pick them on any deployment instead of pasting tokens"
          >
            <For each={ACCOUNTS}>
              {(a) => {
                const p = PROVIDERS.find((x) => x.value === a.provider);
                return (
                  <div class="list-i">
                    <span class="prov">{p?.badge}</span>
                    <div class="nm grow">
                      <b>{p?.label}</b>
                      <span class="plain">{a.description}</span>
                    </div>
                    <Show
                      when={connected(a.kinds)}
                      fallback={
                        <Button size="sm" onClick={() => setAdding(a.provider)}>
                          Connect
                        </Button>
                      }
                    >
                      <Pill tone="ok" dot>
                        Connected
                      </Pill>
                    </Show>
                  </div>
                );
              }}
            </For>
            {footer(finish, true)}
          </Card>
        </Match>

        <Match when={step() >= 3}>
          <Card>
            <EmptyState
              icon="check"
              tone="success"
              title="This server is ready"
              description="You can change all of this later under Settings, Proxy and Identities."
              actions={
                <>
                  <Button variant="primary" href="/deployments/new">
                    <Icon name="plus" />
                    New deployment
                  </Button>
                  <Button href="/">Go to dashboard</Button>
                </>
              }
            />
          </Card>
        </Match>
      </Switch>

      <Show when={adding()}>
        {(provider) => (
          <Show
            when={provider() === "certificate"}
            fallback={
              <AddIdentity
                provider={provider()}
                onCertificate={() => setAdding("certificate")}
                onClose={() => setAdding(null)}
                onSaved={() => {
                  setAdding(null);
                  refetchIdentities();
                }}
              />
            }
          >
            <AddCertificate
              onClose={() => setAdding(null)}
              onSaved={() => {
                setAdding(null);
                refetchIdentities();
              }}
            />
          </Show>
        )}
      </Show>
    </div>
  );
}
