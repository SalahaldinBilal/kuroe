import { A, useSearchParams } from "@solidjs/router";
import { createResource, createSignal, For, Show } from "solid-js";
import { api, type Identity } from "../api";
import { AddCertificate } from "../components/AddCertificate";
import { AddIdentity } from "../components/AddIdentity";
import { Card, Note, PageHeader, Pill } from "../components/ui/Blocks";
import Button from "../components/ui/Button/Button";
import EmptyState from "../components/ui/EmptyState/EmptyState";
import Field from "../components/ui/Field/Field";
import { Icon } from "../components/ui/Icon";
import Modal from "../components/ui/Modal/Modal";
import { ready } from "../components/ui/Overlay";
import Select from "../components/ui/Select/Select";
import { Busy, SkeletonEmpty, SkeletonList } from "../components/ui/Skeleton";
import Toggle from "../components/ui/Switch/Switch";
import { BADGE, expiryWarning, type Group, groupOf, KIND_LABEL, type Provider, scopeLine } from "../identities";
import { rowsFor, useShape } from "../shape";
import useToastState, { errorMessage } from "../states/toastState";

const GROUPS: { group: Group; title: string; description: string; add: Provider | "" }[] = [
  { group: "git", title: "Git hosts", description: "Clone private repositories", add: "" },
  { group: "registry", title: "Container registries", description: "Pull private images", add: "registry" },
  { group: "dns", title: "DNS", description: "Prove domain ownership and make origin certificates", add: "cloudflare" },
  { group: "certs", title: "Certificates", description: "TLS certificates for your domains", add: "certificate" },
];

export function Identities() {
  const { pushToast, pushError } = useToastState;
  const [search, setSearch] = useSearchParams<{ error?: string }>();
  const [list, { refetch }] = createResource(api.identities);
  const [adding, setAdding] = createSignal<Provider | "" | null>(null);
  const [removing, setRemoving] = createSignal<Identity | null>(null);
  const [revoke, setRevoke] = createSignal(false);
  const [pulls, setPulls] = createSignal<{ cert: Identity; on: boolean } | null>(null);
  const [pullsLogin, setPullsLogin] = createSignal("");
  const [testing, setTesting] = createSignal("");
  const cloudflareLogins = () => (ready(list) ?? []).filter((i) => i.kind === "cloudflare");
  const known = useShape<number[]>("identities", [1], () => {
    const all = list.latest;
    if (!all) return undefined;
    return GROUPS.map((g) => all.filter((i) => groupOf(i.kind) === g.group).length).filter((n) => n > 0);
  });

  async function savePulls() {
    const p = pulls();
    if (!p) return;
    try {
      await api.setOriginPulls(p.cert.id, p.on, p.on ? pullsLogin() || null : null);
      pushToast(p.on ? `Origin pulls on for ${p.cert.name}` : `Origin pulls off for ${p.cert.name}`);
      setPulls(null);
      refetch();
    } catch (err) {
      pushError(err);
    }
  }

  async function test(identity: Identity) {
    setTesting(identity.id);
    try {
      const result = await api.testIdentity(identity.id);
      pushToast(`${identity.name}: ${result.message}`);
      refetch();
    } catch (err) {
      pushError(`${identity.name}: ${errorMessage(err)}`);
    } finally {
      setTesting("");
    }
  }

  async function remove(identity: Identity) {
    try {
      await api.removeIdentity(identity.id, revoke());
      pushToast(`Removed ${identity.name}`);
      setRemoving(null);
      refetch();
    } catch (err) {
      pushError(err);
    }
  }

  return (
    <div class="content narrow">
      <PageHeader
        title="Identities"
        description="Accounts kuroe uses to pull code and images and to manage DNS. Add once, pick on any deployment."
        actions={
          <Button variant="primary" onClick={() => setAdding("")}>
            <Icon name="plus" />
            Add identity
          </Button>
        }
      />
      <div class="stack">
        <Show when={search.error}>
          {(message) => (
            <Note
              tone="bad"
              action={
                <Button size="sm" variant="ghost" onClick={() => setSearch({ error: undefined }, { replace: true })}>
                  Dismiss
                </Button>
              }
            >
              {message()}
            </Note>
          )}
        </Show>
        <Show
          when={ready(list)}
          fallback={
            <Show
              when={list.error}
              fallback={
                <Busy>
                  <Show
                    when={known.length > 0}
                    fallback={
                      <section class="card">
                        <SkeletonEmpty action />
                      </section>
                    }
                  >
                    <div class="stack">
                      <For each={known}>{(rows) => <SkeletonList rows={rowsFor(rows)} description actions={2} />}</For>
                    </div>
                  </Show>
                </Busy>
              }
            >
              <Card>
                <EmptyState
                  icon="alert"
                  tone="danger"
                  title="Couldn't load identities"
                  description={String(list.error?.message)}
                />
              </Card>
            </Show>
          }
        >
          {(all) => (
            <Show
              when={all().length > 0}
              fallback={
                <Card>
                  <EmptyState
                    icon="key"
                    title="No identities yet"
                    description="Add a git host, a registry, a DNS account or a certificate. Each kind gets its own section once it has one."
                    actions={
                      <Button variant="primary" onClick={() => setAdding("")}>
                        <Icon name="plus" />
                        Add identity
                      </Button>
                    }
                  />
                </Card>
              }
            >
              <For each={GROUPS.filter((g) => all().some((i) => groupOf(i.kind) === g.group))}>
                {(g) => {
                  const items = () => all().filter((i) => groupOf(i.kind) === g.group);
                  return (
                    <Card
                      title={g.title}
                      description={g.description}
                      actions={
                        <div class="hrow">
                          <span class="count">{items().length}</span>
                          <Button
                            size="sm"
                            variant="ghost"
                            tooltip={`Add to ${g.title}`}
                            onClick={() => setAdding(g.add)}
                          >
                            <Icon name="plus" />
                            Add
                          </Button>
                        </div>
                      }
                    >
                      <For each={items()}>
                        {(identity) => (
                          <div class="list-i">
                            <span class="prov">{BADGE[identity.kind]}</span>
                            <div class="nm grow">
                              <b>{identity.name}</b>
                              <span class="plain">
                                {KIND_LABEL[identity.kind]} · {scopeLine(identity)}
                              </span>
                            </div>
                            <Show when={expiryWarning(identity)}>
                              {(warning) => (
                                <Pill tone={warning() === "Expired" ? "bad" : "warn"} dot>
                                  {warning()}
                                </Pill>
                              )}
                            </Show>
                            <span class="hrow wrap hide-sm" style={{ gap: "4px" }}>
                              <For each={identity.used_by} fallback={<span class="faint">Not used</span>}>
                                {(u) => (
                                  <A class="chip" href={u.id ? `/deployments/${u.id}` : "/proxy"}>
                                    {u.name}
                                  </A>
                                )}
                              </For>
                            </span>
                            <Show when={identity.kind === "certificate"}>
                              <Toggle
                                label="Origin pulls"
                                tooltip="Only accept connections that carry Cloudflare's client certificate"
                                checked={identity.meta.aop === true}
                                onChange={(on) => {
                                  setPullsLogin(String(identity.meta.cloudflare_identity ?? ""));
                                  setPulls({ cert: identity, on });
                                }}
                              />
                            </Show>
                            <Show when={identity.kind !== "git"}>
                              <Button size="sm" disabled={testing() === identity.id} onClick={() => test(identity)}>
                                Test
                              </Button>
                            </Show>
                            <Button
                              variant="ghost"
                              icon
                              size="sm"
                              tooltip="Remove"
                              onClick={() => setRemoving(identity)}
                            >
                              <Icon name="trash" />
                            </Button>
                          </div>
                        )}
                      </For>
                    </Card>
                  );
                }}
              </For>
            </Show>
          )}
        </Show>
      </div>

      <Show when={adding() !== null}>
        <Show
          when={adding() === "certificate"}
          fallback={
            <AddIdentity
              provider={adding() || null}
              onCertificate={() => setAdding("certificate")}
              onClose={() => setAdding(null)}
              onSaved={() => {
                setAdding(null);
                refetch();
              }}
            />
          }
        >
          <AddCertificate
            onBack={() => setAdding("")}
            onClose={() => setAdding(null)}
            onSaved={() => {
              setAdding(null);
              refetch();
            }}
          />
        </Show>
      </Show>

      <Modal
        show={pulls() !== null}
        title={
          pulls()?.on
            ? `Turn on origin pulls for ${pulls()?.cert.name}?`
            : `Turn off origin pulls for ${pulls()?.cert.name}?`
        }
        onHide={() => setPulls(null)}
        footer={
          <>
            <Button variant="ghost" onClick={() => setPulls(null)}>
              Cancel
            </Button>
            <Button variant="primary" onClick={savePulls}>
              {pulls()?.on ? "Turn on" : "Turn off"}
            </Button>
          </>
        }
      >
        <Show
          when={pulls()?.on}
          fallback={<p>Traefik stops asking for Cloudflare's client certificate on these domains.</p>}
        >
          <div class="stack" style={{ gap: "14px" }}>
            <Note tone="warn">
              Turn on Authenticated Origin Pulls for the zone in Cloudflare first, or these domains stop answering. Only
              Cloudflare can reach them afterwards, not the server's IP.
            </Note>
            <Show when={cloudflareLogins().length > 0}>
              <Field label="Turn it on in Cloudflare too" hint="Uses this identity to switch the zone setting on.">
                <Select
                  label="Cloudflare identity"
                  items={[
                    { value: "", label: "No, I did it myself" },
                    ...cloudflareLogins().map((l) => ({ value: l.id, label: l.name })),
                  ]}
                  value={pullsLogin()}
                  onChange={setPullsLogin}
                />
              </Field>
            </Show>
          </div>
        </Show>
      </Modal>

      <Modal
        show={removing() !== null}
        title={`Remove ${removing()?.name}?`}
        onHide={() => {
          setRemoving(null);
          setRevoke(false);
        }}
        footer={
          <>
            <Button variant="ghost" onClick={() => setRemoving(null)}>
              Cancel
            </Button>
            <Button variant="danger-solid" onClick={() => removing() && remove(removing() as Identity)}>
              Remove
            </Button>
          </>
        }
      >
        <p>
          kuroe forgets the saved token. Deployments that still use it have to switch first.
          <Show when={removing()?.kind === "github_app"}>
            {" "}
            The app itself stays on GitHub until you delete it there.
          </Show>
        </p>
        <Show when={removing()?.meta.source === "cloudflare"}>
          <Toggle label="Also revoke it at Cloudflare" checked={revoke()} onChange={setRevoke} />
        </Show>
      </Modal>
    </div>
  );
}
