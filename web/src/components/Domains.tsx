import { createResource, createSignal, For, Show } from "solid-js";
import { api, type DeploymentDetail, type Domain, type Snippet, type WebServerKind } from "../api";
import { certificatesFor, servedLabel } from "../certificates";
import { formatAgo, formatTime } from "../format";
import { type RoutingDraft, routingDraft, routingOf, routingProblem, routingSummary } from "../routing";
import { rowsFor, useShape } from "../shape";
import { isCompose, isStatic } from "../spec";
import useToastState from "../states/toastState";
import { DomainRouting } from "./DomainRouting";
import { Card, Note, Pill } from "./ui/Blocks";
import Button from "./ui/Button/Button";
import EmptyState from "./ui/EmptyState/EmptyState";
import Field from "./ui/Field/Field";
import { Icon } from "./ui/Icon";
import Input from "./ui/Input/Input";
import Modal from "./ui/Modal/Modal";
import { ready } from "./ui/Overlay";
import Select from "./ui/Select/Select";
import { Busy, SkeletonEmpty, SkeletonTable } from "./ui/Skeleton";
import Switch from "./ui/Switch/Switch";
import Tip from "./ui/Tooltip/Tip";
import { ConfigFiles } from "./WebServer";

interface Props {
  detail: DeploymentDetail;
}

export function Domains(props: Props) {
  const { pushToast, pushError } = useToastState;
  const [domains, { refetch }] = createResource(() => props.detail.id, api.domains);
  const [proxy] = createResource(api.proxy);
  const [open, setOpen] = createSignal<{ id: string; snippets: Snippet[]; kind: WebServerKind } | null>(null);
  const [verdict, setVerdict] = createSignal<Record<string, { ok: boolean; message: string }>>({});
  const [adding, setAdding] = createSignal(false);
  const [editing, setEditing] = createSignal<Domain | null>(null);
  const [form, setForm] = createSignal({ hostname: "", tls: true, certificate: "" });
  const [routing, setRouting] = createSignal<RoutingDraft>(routingDraft(null, "", ""));
  const [identities] = createResource(api.identities);
  const saved = () => (ready(identities) ?? []).filter((i) => i.kind === "certificate");
  const pinItems = (host: string) => [
    { value: "", label: "Automatic" },
    ...certificatesFor(saved(), host).map((c) => ({ value: c.id, label: c.name })),
    { value: "acme", label: "Let's Encrypt" },
  ];
  const [busy, setBusy] = createSignal(false);

  const site = () => isStatic(props.detail.spec);
  const services = () => {
    if (site()) return ["site"];
    const names = new Set<string>();
    for (const c of props.detail.containers) if (c.service) names.add(c.service);
    for (const name of Object.keys(props.detail.spec.services)) names.add(name);
    if (!isCompose(props.detail.spec)) names.add("app");
    return [...names].sort();
  };
  const mode = () => ready(proxy)?.settings.mode;
  const external = () => mode() === "external";
  const managed = () => mode() === "managed";
  const tlsInFront = () => {
    const s = ready(proxy)?.settings;
    return s?.mode === "managed" && s.bind === "behind" && (s.web_server?.mode ?? "front") === "front";
  };
  const list = () => ready(domains) ?? [];

  const defaultPort = () => (site() ? "80" : "");

  function startAdd() {
    setEditing(null);
    setForm({ hostname: "", tls: true, certificate: "" });
    setRouting(routingDraft(null, services()[0] ?? "", defaultPort()));
    setAdding(true);
  }

  function startEdit(d: Domain) {
    setEditing(d);
    setRouting(routingDraft(d, services()[0] ?? "", defaultPort()));
    setAdding(true);
  }

  async function save() {
    const f = form();
    const { service, port, options } = routingOf(routing());
    const current = editing();
    setBusy(true);
    try {
      if (current) {
        await api.updateDomain(current.id, service, port, options);
        pushToast(`Saved ${current.hostname}`);
      } else {
        await api.addDomain(props.detail.id, f.hostname.trim(), service, port, f.tls, f.certificate || null, options);
        pushToast(`Added ${f.hostname.trim()}`);
      }
      setAdding(false);
      refetch();
    } catch (err) {
      pushError(err);
    } finally {
      setBusy(false);
    }
  }

  const problem = () => routingProblem(routing(), site());
  const canSave = () => !busy() && !problem() && (!!editing() || !!form().hostname.trim());

  async function showSnippets(id: string) {
    if (open()?.id === id) return setOpen(null);
    try {
      const kind = ready(proxy)?.settings.web_server?.kind ?? "nginx";
      setOpen({ id, snippets: await api.snippets(id), kind });
    } catch (err) {
      pushError(err);
    }
  }

  async function verify(id: string) {
    try {
      const result = await api.verifyDomain(id);
      setVerdict({ ...verdict(), [id]: result });
      if (result.ok) refetch();
    } catch (err) {
      pushError(err);
    }
  }

  async function pin(d: Domain, certificate: string) {
    try {
      await api.pinCertificate(d.id, certificate || null);
      pushToast(
        `${d.hostname} now uses ${certificate ? pinItems(d.hostname).find((i) => i.value === certificate)?.label : "the automatic pick"}`,
      );
      refetch();
    } catch (err) {
      pushError(err);
    }
  }

  async function remove(id: string, hostname: string) {
    try {
      await api.removeDomain(id);
      pushToast(`Removed ${hostname}`);
      refetch();
    } catch (err) {
      pushError(err);
    }
  }

  const canEdit = () => props.detail.permissions.includes("dep.domains");

  const addButton = (primary: boolean) => (
    <Show when={canEdit()}>
      <Button variant={primary ? "primary" : "subtle"} size="sm" onClick={startAdd}>
        <Icon name="plus" />
        Add domain
      </Button>
    </Show>
  );

  const loading = () => (!proxy.latest && !proxy.error) || (!domains.latest && !domains.error);
  const known = useShape("domains", { off: false, rows: 0 }, () =>
    loading() ? undefined : { off: mode() === "off", rows: list().length },
  );

  return (
    <Show
      when={loading() ? !known.off : mode() !== "off"}
      fallback={
        <Card>
          <EmptyState
            icon="globe"
            title="The proxy is off"
            description="Turn it on to route domains to this deployment."
            actions={
              <Button variant="primary" size="sm" href="/proxy">
                Open proxy settings
              </Button>
            }
          />
        </Card>
      }
    >
      <Card
        title="Domains"
        description={
          external()
            ? "You run the proxy. kuroe gives you the config and checks it works."
            : tlsInFront()
              ? "Your web server handles HTTPS and sends these to Traefik."
              : "Managed Traefik routes these and gets the certificates."
        }
        actions={addButton(true)}
      >
        <Show
          when={list().length > 0}
          fallback={
            <Show
              when={!loading()}
              fallback={
                <Busy>
                  <Show when={known.rows > 0} fallback={<SkeletonEmpty action={canEdit()} />}>
                    <SkeletonTable bare rows={rowsFor(known.rows)} columns={5} />
                  </Show>
                </Busy>
              }
            >
              <EmptyState
                icon="globe"
                title="No domains yet"
                description="Point a hostname at this server, then add it here."
                actions={addButton(false)}
              />
            </Show>
          }
        >
          <table class="tbl">
            <thead>
              <tr>
                <th>Hostname</th>
                <th>Target</th>
                <th>Protocol</th>
                <Show when={!external()}>
                  <th class="hide-sm">Certificate</th>
                </Show>
                <Show when={external()}>
                  <th class="hide-sm">Verified</th>
                </Show>
                <th />
              </tr>
            </thead>
            <tbody>
              <For each={list()}>
                {(d) => (
                  <>
                    <tr>
                      <td>
                        <a
                          class="hrow"
                          style={{ gap: "6px", color: "var(--ink)", "font-weight": "500" }}
                          href={`${d.tls ? "https" : "http"}://${d.hostname}/`}
                          target="_blank"
                          rel="noreferrer"
                        >
                          {d.hostname}
                          <Icon name="ext" class="faint" />
                        </a>
                      </td>
                      <td>
                        <Show
                          when={d.options.redirect}
                          fallback={
                            <div class="hrow wrap">
                              <span class="chip">{site() ? "static files" : `${d.service}:${d.port}`}</span>
                              <Show when={routingSummary(d.options)}>
                                {(summary) => <span class="hint">{summary()}</span>}
                              </Show>
                            </div>
                          }
                        >
                          {(target) => (
                            <span class="chip">
                              <Icon name="right" />
                              {target()}
                            </span>
                          )}
                        </Show>
                      </td>
                      <td>
                        <Pill tone={d.tls ? "ok" : undefined} dot>
                          {d.tls ? "HTTPS" : "HTTP"}
                        </Pill>
                      </td>
                      <Show when={!external()}>
                        <td class="hide-sm">
                          <Show when={d.tls} fallback={<span class="faint">None</span>}>
                            <Show
                              when={canEdit() && d.served_by.kind !== "server"}
                              fallback={<span class="muted">{servedLabel(d.served_by)}</span>}
                            >
                              <div class="hrow wrap">
                                <Select
                                  size="sm"
                                  label={`Certificate for ${d.hostname}`}
                                  items={pinItems(d.hostname)}
                                  value={d.certificate ?? ""}
                                  onChange={(certificate) => pin(d, certificate)}
                                />
                                <span class="hint">{servedLabel(d.served_by)}</span>
                              </div>
                            </Show>
                          </Show>
                        </td>
                      </Show>
                      <Show when={external()}>
                        <td class="hide-sm">
                          <Show when={d.verified_at} fallback={<span class="faint">Not verified</span>}>
                            {(at) => (
                              <Tip content={`${formatTime(at())} UTC`}>
                                <span class="muted">{formatAgo(at())}</span>
                              </Tip>
                            )}
                          </Show>
                        </td>
                      </Show>
                      <td class="acts">
                        <Show when={external()}>
                          <Button size="sm" active={open()?.id === d.id} onClick={() => showSnippets(d.id)}>
                            <Icon name="code" />
                            Config
                          </Button>
                          <Button size="sm" onClick={() => verify(d.id)}>
                            Verify
                          </Button>
                        </Show>
                        <Show when={canEdit()}>
                          <Button variant="ghost" icon size="sm" tooltip="Edit routing" onClick={() => startEdit(d)}>
                            <Icon name="sliders" />
                          </Button>
                          <Button
                            variant="ghost"
                            icon
                            size="sm"
                            tooltip="Remove"
                            onClick={() => remove(d.id, d.hostname)}
                          >
                            <Icon name="trash" />
                          </Button>
                        </Show>
                      </td>
                    </tr>
                    <Show when={verdict()[d.id]}>
                      {(v) => (
                        <tr>
                          <td colspan={6} class="sub-row">
                            <Note tone={v().ok ? "ok" : "bad"}>{v().message}</Note>
                          </td>
                        </tr>
                      )}
                    </Show>
                    <Show when={open()?.id === d.id && open()}>
                      {(o) => (
                        <tr>
                          <td colspan={6} class="sub-row">
                            <div class="stack" style={{ gap: "10px" }}>
                              <div class="hrow wrap">
                                <Select
                                  size="sm"
                                  label="Web server"
                                  items={o().snippets.map((s) => ({ value: s.kind, label: s.label }))}
                                  value={o().kind}
                                  onChange={(kind) => setOpen({ ...o(), kind })}
                                />
                                <span class="grow" />
                                <span class="hint">Add this to your web server, then press Verify.</span>
                              </div>
                              <Show when={o().snippets.find((s) => s.kind === o().kind)}>
                                {(s) => <ConfigFiles files={s().files} steps={s().steps} />}
                              </Show>
                            </div>
                          </td>
                        </tr>
                      )}
                    </Show>
                  </>
                )}
              </For>
            </tbody>
          </table>
        </Show>
      </Card>

      <Modal
        show={adding()}
        title={editing() ? `Routing for ${editing()?.hostname}` : "Add domain"}
        width={managed() && !site() ? 680 : undefined}
        onHide={() => setAdding(false)}
        footer={
          <>
            <Show when={problem()}>{(text) => <span class="hint grow">{text()}</span>}</Show>
            <Button variant="ghost" onClick={() => setAdding(false)}>
              Cancel
            </Button>
            <Button variant="primary" disabled={!canSave()} onClick={save}>
              {editing() ? "Save" : "Add domain"}
            </Button>
          </>
        }
      >
        <Show when={!editing()}>
          <Field label="Hostname" hint="Point its DNS record at this server first.">
            <Input
              mono
              autofocus
              label="Hostname"
              placeholder="app.example.com"
              value={form().hostname}
              onValue={(hostname) => setForm({ ...form(), hostname })}
            />
          </Field>
        </Show>
        <DomainRouting
          draft={routing()}
          onChange={setRouting}
          services={services()}
          site={site()}
          managed={managed()}
        />
        <Show when={!editing()}>
          <Switch label="HTTPS" checked={form().tls} onChange={(tls) => setForm({ ...form(), tls })} />
        </Show>
        <Show when={!editing() && form().tls && !external() && !tlsInFront()}>
          <Field
            label="Certificate"
            hint="Automatic uses a saved certificate that covers the hostname, otherwise Let's Encrypt."
          >
            <Select
              label="Certificate"
              items={pinItems(form().hostname.trim())}
              value={form().certificate}
              onChange={(certificate) => setForm({ ...form(), certificate })}
            />
          </Field>
        </Show>
      </Modal>
    </Show>
  );
}
