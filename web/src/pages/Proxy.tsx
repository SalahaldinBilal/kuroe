import { createEffect, createMemo, createResource, createSignal, Show } from "solid-js";
import { useUser } from "../App";
import { api, type CertMode, can, type PortHolder, type ProxyMode, type ProxySettings } from "../api";
import { Certificates } from "../components/Certificates";
import { OwnDomain } from "../components/OwnDomain";
import { Card, Note, PageHeader, Pill } from "../components/ui/Blocks";
import Button from "../components/ui/Button/Button";
import EmptyState from "../components/ui/EmptyState/EmptyState";
import Input, { Unit } from "../components/ui/Input/Input";
import { ready, SaveBar, same } from "../components/ui/Overlay";
import SegmentedControl from "../components/ui/SegmentedControl/SegmentedControl";
import SettingRow from "../components/ui/SettingRow/SettingRow";
import { Busy, SkeletonSettings } from "../components/ui/Skeleton";
import Switch from "../components/ui/Switch/Switch";
import { WebServerCard } from "../components/WebServer";
import { useShape } from "../shape";
import useToastState from "../states/toastState";
import { serverDefaults, serverDraft, serverOf, WEB_SERVERS, type WebServerDraft } from "../webserver";

export interface Draft {
  mode: ProxyMode;
  cert_mode: CertMode;
  dns_identity: string;
  dns_wildcard: boolean;
  acme_email: string;
  http_port: string;
  https_port: string;
  drain_seconds: string;
  bind: "ports" | "behind";
  behind_http: string;
  behind_https: string;
  web_server: WebServerDraft | null;
  access_log: boolean;
}

const MODE_HINT: Record<ProxyMode, string> = {
  off: "Ports stay on 127.0.0.1 and deployments have no domains.",
  managed: "kuroe runs Traefik in front of your deployments and keeps its routes up to date.",
  external: "You run nginx, Apache, Caddy or another server. kuroe gives you its config.",
};

function draftOf(s: ProxySettings): Draft {
  return {
    mode: s.mode,
    cert_mode: s.cert_mode,
    dns_identity: s.dns_identity ?? "",
    dns_wildcard: s.dns_wildcard,
    acme_email: s.acme_email ?? "",
    http_port: String(s.http_port),
    https_port: String(s.https_port),
    drain_seconds: String(s.drain_seconds),
    bind: s.bind,
    behind_http: s.behind_http,
    behind_https: s.behind_https,
    web_server: s.web_server ? serverDraft(s.web_server) : null,
    access_log: !!s.access_log,
  };
}

function number(label: string, text: string): number {
  const n = Number(text.trim());
  if (!Number.isInteger(n) || n < 0) throw new Error(`${label} must be a whole number`);
  return n;
}

function settingsOf(d: Draft): ProxySettings {
  return {
    mode: d.mode,
    acme_email: d.acme_email.trim() || null,
    http_port: number("HTTP port", d.http_port),
    https_port: number("HTTPS port", d.https_port),
    drain_seconds: number("Drain time", d.drain_seconds),
    cert_mode: d.cert_mode,
    dns_identity: d.dns_identity || null,
    dns_wildcard: d.dns_wildcard,
    bind: d.bind,
    behind_http: d.behind_http.trim(),
    behind_https: d.behind_https.trim(),
    web_server: d.web_server ? serverOf(d.web_server) : null,
    access_log: d.access_log,
  };
}

/** The web server holding the ports, when it's one kuroe knows, with its container. */
function detected(holders: PortHolder[]): PortHolder | undefined {
  return holders.find((h) => h.by) ?? holders[0];
}

export function ProxyPage() {
  const user = useUser();
  const { pushToast, pushError } = useToastState;
  const [data, { refetch }] = createResource(api.proxy);
  const canWrite = () => can(user(), "proxy");
  const [ports, { refetch: recheck }] = createResource(
    () => canWrite() || undefined,
    () => api.proxyPorts(),
  );
  const baseline = createMemo(() => {
    const d = ready(data);
    return d ? draftOf(d.settings) : null;
  });
  const [draft, setDraft] = createSignal<Draft | null>(null);
  const [busy, setBusy] = createSignal(false);
  const dirty = () => draft() !== null && !same(draft(), baseline());

  createEffect(() => {
    const b = baseline();
    if (b && !draft()) setDraft(b);
  });

  function change(patch: Partial<Draft>) {
    const d = draft();
    if (d) setDraft({ ...d, ...patch });
  }

  const holders = () => ready(ports)?.holders ?? [];
  const blocked = () => {
    const d = draft();
    return d?.mode === "managed" && d.bind === "ports" && holders().length > 0;
  };
  const holder = () => detected(holders());
  const holderName = () => {
    const by = holder()?.by;
    return by ? WEB_SERVERS[by].label : "it";
  };

  function useServer(mode: "behind" | "external") {
    const h = holder();
    const web_server = h?.by ? serverDefaults(h.by, h.container) : null;
    change(mode === "behind" ? { bind: "behind", web_server } : { mode: "external", web_server });
  }

  /** The web server terminates TLS, so Traefik doesn't need certificates. */
  const tlsInFront = (d: Draft) => d.bind === "behind" && (d.web_server === null || d.web_server.mode === "front");

  const known = useShape("proxy", { mode: "managed" as ProxyMode, server: false, certificates: true }, () => {
    const b = baseline();
    if (!b) return undefined;
    const server = b.mode === "external" || (b.mode === "managed" && b.bind === "behind");
    return { mode: b.mode, server, certificates: b.mode === "managed" && !tlsInFront(b) };
  });

  async function save() {
    const d = draft();
    if (!d) return;
    setBusy(true);
    try {
      await api.saveProxy(settingsOf(d));
      pushToast("Proxy settings saved");
      await refetch();
      setDraft(baseline());
      recheck();
    } catch (err) {
      pushError(err);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div class="content narrow">
      <PageHeader title="Proxy" description="How traffic from the internet reaches your deployments" />
      <Show
        when={draft()}
        fallback={
          <Show
            when={data.error}
            fallback={
              <Busy>
                <div class="stack">
                  <SkeletonSettings lines={known.mode === "managed" ? [1, 0, 0, 0, 2] : [1]} />
                  <Show when={known.mode === "external"}>
                    <span class="bone note-bone" />
                  </Show>
                  <Show when={known.mode !== "off" && known.server}>
                    <SkeletonSettings rows={3} description />
                  </Show>
                  <Show when={known.mode === "managed" && known.certificates}>
                    <SkeletonSettings lines={[1, 1, 1]} description />
                  </Show>
                </div>
              </Busy>
            }
          >
            <Card>
              <EmptyState
                icon="alert"
                tone="danger"
                title="Couldn't load the proxy settings"
                description={String(data.error?.message)}
              />
            </Card>
          </Show>
        }
      >
        {(d) => (
          <div class="stack">
            <Show when={blocked()}>
              <Note tone="warn">
                <div class="stack" style={{ gap: "8px" }}>
                  <span>
                    {ready(ports)?.message}, so Traefik can't start. Run Traefik behind {holderName()}, or let kuroe
                    write {holderName() === "it" ? "its" : holderName()} configs.
                  </span>
                  <div class="hrow wrap" style={{ gap: "8px" }}>
                    <Button size="sm" disabled={!canWrite()} onClick={() => useServer("behind")}>
                      Run Traefik behind {holderName()}
                    </Button>
                    <Button size="sm" disabled={!canWrite()} onClick={() => useServer("external")}>
                      Let kuroe write {holderName() === "it" ? "its" : holderName()} configs
                    </Button>
                  </div>
                </div>
              </Note>
            </Show>
            <Card
              title="Proxy"
              actions={
                <Show when={d().mode === "managed" ? ready(data) : undefined}>
                  {(state) => (
                    <Pill tone={state().traefik_running ? "ok" : "bad"} dot>
                      {state().traefik_running ? "Traefik running" : "Traefik not running"}
                    </Pill>
                  )}
                </Show>
              }
            >
              <SettingRow label="Mode" description={MODE_HINT[d().mode]}>
                <SegmentedControl
                  label="Mode"
                  value={d().mode}
                  onChange={(mode) => change({ mode })}
                  options={
                    [
                      { value: "off", label: "Off" },
                      { value: "managed", label: "Managed Traefik" },
                      { value: "external", label: "My own proxy" },
                    ] as const
                  }
                />
              </SettingRow>
              <Show when={d().mode === "managed"}>
                <SettingRow label="Where Traefik listens">
                  <SegmentedControl
                    label="Where Traefik listens"
                    value={d().bind}
                    onChange={(bind) => change({ bind })}
                    options={
                      [
                        { value: "ports", label: "On its own ports" },
                        { value: "behind", label: "Behind a web server" },
                      ] as const
                    }
                  />
                </SettingRow>
                <Show
                  when={d().bind === "behind"}
                  fallback={
                    <>
                      <SettingRow label="HTTP port">
                        <Input
                          mono
                          inputMode="numeric"
                          label="HTTP port"
                          disabled={!canWrite()}
                          value={d().http_port}
                          onValue={(http_port) => change({ http_port })}
                        />
                      </SettingRow>
                      <SettingRow label="HTTPS port">
                        <Input
                          mono
                          inputMode="numeric"
                          label="HTTPS port"
                          disabled={!canWrite()}
                          value={d().https_port}
                          onValue={(https_port) => change({ https_port })}
                        />
                      </SettingRow>
                    </>
                  }
                >
                  <SettingRow label="HTTP address" description="Where the web server sends plain HTTP.">
                    <Input
                      mono
                      label="HTTP address"
                      disabled={!canWrite()}
                      value={d().behind_http}
                      onValue={(behind_http) => change({ behind_http })}
                    />
                  </SettingRow>
                  <SettingRow label="HTTPS address" description="Where HTTPS arrives when Traefik handles it.">
                    <Input
                      mono
                      label="HTTPS address"
                      disabled={!canWrite()}
                      value={d().behind_https}
                      onValue={(behind_https) => change({ behind_https })}
                    />
                  </SettingRow>
                </Show>
                <SettingRow label="Drain time" description="How long to keep the old version after switching traffic.">
                  <Input
                    mono
                    inputMode="numeric"
                    label="Drain time"
                    disabled={!canWrite()}
                    value={d().drain_seconds}
                    onValue={(drain_seconds) => change({ drain_seconds })}
                    after={<Unit>seconds</Unit>}
                  />
                </SettingRow>
                <SettingRow
                  label="Access log"
                  description="Traefik writes a line for every request to the kuroe_traefik container's output."
                >
                  <Switch
                    checked={d().access_log}
                    disabled={!canWrite()}
                    onChange={(access_log) => change({ access_log })}
                  />
                </SettingRow>
              </Show>
            </Card>
            <Show when={ready(data)?.settings.mode !== "off" ? ready(data) : undefined} keyed>
              {(state) => (
                <OwnDomain saved={state.ui} mode={state.settings.mode} disabled={!canWrite()} onSaved={refetch} />
              )}
            </Show>
            <Show when={d().mode === "managed"}>
              <Show when={d().bind === "behind"}>
                <WebServerCard
                  behind
                  server={d().web_server}
                  behindHttp={d().behind_http}
                  disabled={!canWrite()}
                  saved={!dirty()}
                  error={ready(data)?.web_server_error ?? null}
                  settings={() => settingsOf(d())}
                  onChange={(web_server) => change({ web_server })}
                  onInstalled={refetch}
                />
              </Show>
              <Show when={!tlsInFront(d())}>
                <Certificates draft={d()} disabled={!canWrite()} onChange={change} />
              </Show>
            </Show>
            <Show when={d().mode === "external"}>
              <Note>
                Ports stay on 127.0.0.1. Add a domain on a deployment to get its config for your web server, then check
                it with Verify.
              </Note>
              <WebServerCard
                behind={false}
                server={d().web_server}
                behindHttp={d().behind_http}
                disabled={!canWrite()}
                saved={!dirty()}
                error={ready(data)?.web_server_error ?? null}
                settings={() => settingsOf(d())}
                onChange={(web_server) => change({ web_server })}
                onInstalled={refetch}
              />
            </Show>
            <SaveBar
              show={dirty() && canWrite()}
              busy={busy()}
              busyMessage={
                d().mode === "managed"
                  ? "Applying. Starting Traefik downloads its image the first time, about 250 MB, then kuroe waits for it to answer and rewrites the routes."
                  : "Applying the proxy settings and rewriting the routes."
              }
              message="Proxy settings changed."
              onSave={save}
              onDiscard={() => setDraft(baseline())}
            />
          </div>
        )}
      </Show>
    </div>
  );
}
