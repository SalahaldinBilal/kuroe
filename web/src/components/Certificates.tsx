import { A } from "@solidjs/router";
import { createResource, createSignal, For, Show } from "solid-js";
import { api, type CertMode } from "../api";
import { sansOf } from "../certificates";
import { formatDate } from "../format";
import type { Draft } from "../pages/Proxy";
import { AddCertificate } from "./AddCertificate";
import { Card, Note } from "./ui/Blocks";
import Button from "./ui/Button/Button";
import { Icon } from "./ui/Icon";
import Input from "./ui/Input/Input";
import { ready } from "./ui/Overlay";
import Select from "./ui/Select/Select";
import SettingRow from "./ui/SettingRow/SettingRow";
import Toggle from "./ui/Switch/Switch";

const MODES: { value: CertMode; label: string; description: string }[] = [
  { value: "http", label: "Let's Encrypt", description: "Free certificates, checked over port 80." },
  {
    value: "dns",
    label: "Let's Encrypt over DNS",
    description: "Proves ownership with a DNS record. Works behind Cloudflare's proxy, wildcards too.",
  },
  {
    value: "none",
    label: "Only saved certificates",
    description: "Domains without a saved certificate get Traefik's self-signed one, which browsers warn about.",
  },
];

/** How Traefik gets certificates. Saved certificates always win for the hostnames they cover. */
export function Certificates(props: { draft: Draft; disabled: boolean; onChange: (patch: Partial<Draft>) => void }) {
  const [identities, { refetch }] = createResource(api.identities);
  const [adding, setAdding] = createSignal(false);
  const all = () => ready(identities) ?? [];
  const logins = () => all().filter((i) => i.kind === "cloudflare");
  const saved = () => all().filter((i) => i.kind === "certificate");
  const zones = () => {
    const z = logins().find((l) => l.id === props.draft.dns_identity)?.meta.zones;
    return Array.isArray(z) ? (z as string[]) : [];
  };
  const mode = () => MODES.find((m) => m.value === props.draft.cert_mode) ?? MODES[0];
  const waiting = () => props.draft.cert_mode !== "none" && !props.draft.acme_email.trim();

  return (
    <Card title="Certificates" description="Optional. How Traefik gets HTTPS certificates">
      <SettingRow label="Get certificates from" description={mode().description}>
        <Select
          label="Get certificates from"
          disabled={props.disabled}
          items={MODES.map((m) => ({ value: m.value, label: m.label }))}
          value={props.draft.cert_mode}
          onChange={(cert_mode) => props.onChange({ cert_mode })}
        />
      </SettingRow>
      <Show when={props.draft.cert_mode === "dns"}>
        <SettingRow
          label="DNS identity"
          description={
            <>
              A Cloudflare token from{" "}
              <A class="link" href="/identities">
                Identities
              </A>
              .
            </>
          }
        >
          <Select
            label="DNS identity"
            placeholder={logins().length ? "Pick a Cloudflare identity" : "Add a Cloudflare identity first"}
            disabled={props.disabled}
            items={logins().map((l) => ({ value: l.id, label: l.name }))}
            value={props.draft.dns_identity}
            onChange={(dns_identity) => props.onChange({ dns_identity })}
          />
        </SettingRow>
        <SettingRow
          label="Wildcards"
          description={
            zones().length
              ? `One certificate per zone, like ${zones()
                  .slice(0, 2)
                  .map((z) => `*.${z}`)
                  .join(", ")}.`
              : "One certificate per zone instead of one per domain."
          }
        >
          <Toggle
            label="Wildcards"
            checked={props.draft.dns_wildcard}
            disabled={props.disabled}
            onChange={(dns_wildcard) => props.onChange({ dns_wildcard })}
          />
        </SettingRow>
      </Show>
      <Show when={props.draft.cert_mode !== "none"}>
        <SettingRow label="Email" description="Turns Let's Encrypt on. It sends notices about your certificates here.">
          <Input
            type="email"
            label="Let's Encrypt email"
            placeholder="you@example.com"
            disabled={props.disabled}
            value={props.draft.acme_email}
            onValue={(acme_email) => props.onChange({ acme_email })}
          />
        </SettingRow>
      </Show>
      <Show when={waiting()}>
        <div class="card-b">
          <Note>
            Without an email, Let's Encrypt stays off. Domains work over HTTP, and HTTPS uses a self-signed certificate
            until you add an email or a saved certificate.
          </Note>
        </div>
      </Show>
      <SettingRow
        label="Saved certificates"
        description="These win for the hostnames they cover. Pin a domain to one on its Domains tab."
        type="stack"
      >
        <div class="stack" style={{ gap: "6px" }}>
          <For each={saved()} fallback={<span class="faint">None saved yet.</span>}>
            {(c) => (
              <div class="hrow" style={{ gap: "8px" }}>
                <Icon name="lock" />
                <b>{c.name}</b>
                <span class="mono muted grow">{sansOf(c).join(", ")}</span>
                <Show when={c.expires_at}>{(at) => <span class="muted">until {formatDate(at())}</span>}</Show>
              </div>
            )}
          </For>
          <div>
            <Button size="sm" disabled={props.disabled} onClick={() => setAdding(true)}>
              <Icon name="plus" />
              Add a certificate
            </Button>
          </div>
        </div>
      </SettingRow>
      <Show when={adding()}>
        <AddCertificate
          onClose={() => setAdding(false)}
          onSaved={() => {
            setAdding(false);
            refetch();
          }}
        />
      </Show>
    </Card>
  );
}
