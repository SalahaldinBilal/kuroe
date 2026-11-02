import { createResource, createSignal, For, Show } from "solid-js";
import { api, type ConfigFile, type ProxySettings, type WebServerKind, type WebServerPreview } from "../api";
import useToastState from "../states/toastState";
import { extraFile, KINDS, serverDefaults, switchKind, WEB_SERVERS, type WebServerDraft } from "../webserver";
import { Card, Note } from "./ui/Blocks";
import Button from "./ui/Button/Button";
import Field, { FieldRow } from "./ui/Field/Field";
import { Icon } from "./ui/Icon";
import Input from "./ui/Input/Input";
import { CopyButton, ready } from "./ui/Overlay";
import SegmentedControl from "./ui/SegmentedControl/SegmentedControl";
import Select from "./ui/Select/Select";
import SettingRow from "./ui/SettingRow/SettingRow";
import Switch from "./ui/Switch/Switch";

/** Config files with their path and a copy button, plus the lines the user adds to their own config once. */
export function ConfigFiles(props: { files: ConfigFile[]; steps: string[] }) {
  return (
    <div class="stack" style={{ gap: "12px" }}>
      <Show when={props.steps.length > 0}>
        <Note>
          <div class="stack" style={{ gap: "4px" }}>
            <For each={props.steps}>{(step) => <span>{step}</span>}</For>
          </div>
        </Note>
      </Show>
      <For each={props.files}>
        {(file) => (
          <div class="stack" style={{ gap: "6px" }}>
            <div class="hrow" style={{ "justify-content": "space-between", gap: "8px" }}>
              <span class="mono muted">{file.path}</span>
              <CopyButton text={file.body} />
            </div>
            <pre class="code full">{file.body || "(empty)"}</pre>
          </div>
        )}
      </For>
    </div>
  );
}

interface Props {
  server: WebServerDraft | null;
  /** Managed Traefik behind the server. Otherwise external mode, where the server forwards to each deployment. */
  behind: boolean;
  behindHttp: string;
  disabled: boolean;
  /** No unsaved changes, so Install writes what is shown. */
  saved: boolean;
  error: string | null;
  settings: () => ProxySettings;
  onChange: (server: WebServerDraft | null) => void;
  onInstalled: () => void;
}

const OTHER = "other";

const EXTRA_DEFAULT: Partial<Record<WebServerKind, string>> = {
  nginx: "/etc/nginx/kuroe-stream.conf",
  haproxy: "/etc/haproxy/kuroe.map",
};

export function WebServerCard(props: Props) {
  const { pushToast, pushError } = useToastState;
  const [version] = createResource(api.version);
  const [preview, setPreview] = createSignal<WebServerPreview | null>(null);
  const [output, setOutput] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);

  const kuroeInContainer = () => ready(version)?.install.mode === "container";

  function patch(p: Partial<WebServerDraft>) {
    const s = props.server;
    if (s) props.onChange({ ...s, ...p });
    setPreview(null);
  }

  function pick(value: WebServerKind | typeof OTHER) {
    setPreview(null);
    if (value === OTHER) return props.onChange(null);
    props.onChange(props.server ? switchKind(props.server, value) : serverDefaults(value));
  }

  async function showPreview() {
    setBusy(true);
    try {
      setPreview(await api.previewWebServer(props.settings()));
    } catch (err) {
      pushError(err);
    } finally {
      setBusy(false);
    }
  }

  async function install() {
    const s = props.server;
    if (!s) return;
    setBusy(true);
    try {
      const result = await api.installWebServer();
      setOutput(result.output);
      pushToast(`Installed into ${WEB_SERVERS[s.kind].label}`);
    } catch (err) {
      setOutput(null);
      pushError(err);
    } finally {
      setBusy(false);
      props.onInstalled();
    }
  }

  const kindItems = () => [
    ...KINDS.map((k) => ({ value: k as WebServerKind | typeof OTHER, label: WEB_SERVERS[k].label })),
    ...(props.behind ? [{ value: OTHER as WebServerKind | typeof OTHER, label: "Something else" }] : []),
  ];

  return (
    <Card
      title={props.behind ? "Web server in front" : "Write configs for me"}
      description={
        props.behind
          ? "It takes ports 80 and 443 and sends kuroe's hostnames to Traefik."
          : "kuroe writes your web server's config for every domain and reloads it."
      }
      actions={
        <Show when={!props.behind}>
          <Switch
            label="On"
            checked={props.server !== null}
            disabled={props.disabled}
            onChange={(on) => pick(on ? "nginx" : OTHER)}
          />
        </Show>
      }
    >
      <Show when={props.behind || props.server}>
        <SettingRow label="Web server">
          <Select
            label="Web server"
            disabled={props.disabled}
            items={kindItems()}
            value={props.server?.kind ?? OTHER}
            onChange={pick}
          />
        </SettingRow>
        <Show
          when={props.server}
          fallback={
            <div class="card-b">
              <Note>
                Send kuroe's hostnames to <span class="mono">http://{props.behindHttp}</span>. Keep the Host header and
                set X-Forwarded-Proto. Your server handles HTTPS.
              </Note>
            </div>
          }
        >
          {(s) => {
            const info = () => WEB_SERVERS[s().kind];
            const extra = () => extraFile(s().kind, props.behind ? s().mode : "front");
            const front = () => !props.behind || s().mode === "front";
            const manual = () => kuroeInContainer() && s().where === "host";
            return (
              <>
                <Show when={props.behind}>
                  <SettingRow
                    label="HTTPS certificates"
                    description={
                      !info().passthrough
                        ? `${info().label} can't pass HTTPS through untouched, so it keeps its own certificates.`
                        : s().mode === "front"
                          ? `${info().label} keeps its own certificates, for example from certbot, and sends plain HTTP to Traefik.`
                          : `${info().label} passes HTTPS through by hostname. kuroe's Traefik gets the certificates.`
                    }
                  >
                    <Show when={info().passthrough}>
                      <SegmentedControl
                        label="HTTPS certificates"
                        value={s().mode}
                        onChange={(mode) => patch({ mode })}
                        options={[
                          { value: "front" as const, label: `${info().label} has them` },
                          { value: "passthrough" as const, label: "kuroe gets them" },
                        ]}
                      />
                    </Show>
                  </SettingRow>
                </Show>
                <SettingRow
                  label={`${info().label} runs`}
                  description={
                    s().where === "host"
                      ? "Installed on the server itself."
                      : "In its own Docker container on this server."
                  }
                >
                  <SegmentedControl
                    label={`${info().label} runs`}
                    value={s().where}
                    onChange={(where) => patch({ where })}
                    options={
                      [
                        { value: "host", label: "On the server" },
                        { value: "container", label: "In a container" },
                      ] as const
                    }
                  />
                </SettingRow>
                <Show when={manual()}>
                  <div class="card-b">
                    <Note tone="warn">
                      kuroe runs in a container, so it can't write {info().label}'s config on the server or reload it.
                      Use Show config below, copy the file to <span class="mono">{s().path}</span>
                      <Show when={s().command}>
                        {" "}
                        and run <span class="mono">{s().command}</span>
                      </Show>
                      . Do it again when domains change. If {info().label} runs in a container instead, pick that above.
                    </Note>
                  </div>
                </Show>
                <SettingRow
                  type="stack"
                  label="Config file"
                  description={
                    manual()
                      ? "Where the file goes on the server."
                      : s().where === "container"
                        ? `kuroe writes it here. Mount ${info().label}'s config folder into kuroe at the same path.`
                        : "kuroe writes it here and replaces it on every install."
                  }
                >
                  <Input
                    mono
                    label="Config file"
                    disabled={props.disabled}
                    value={s().path}
                    onValue={(path) => patch({ path })}
                  />
                </SettingRow>
                <Show when={extra()}>
                  {(label) => (
                    <SettingRow
                      type="stack"
                      label={label()}
                      description={
                        s().kind === "nginx"
                          ? "The passthrough part of the config. nginx.conf has to include it at the top level, outside the http block."
                          : "Which hostname goes to which backend. kuroe writes it next to the config file."
                      }
                    >
                      <Input
                        mono
                        label={label()}
                        placeholder={EXTRA_DEFAULT[s().kind] ?? ""}
                        disabled={props.disabled}
                        value={s().extra_path}
                        onValue={(extra_path) => patch({ extra_path })}
                      />
                    </SettingRow>
                  )}
                </Show>
                <Show
                  when={s().where === "container"}
                  fallback={
                    <SettingRow
                      type="stack"
                      label="Test and reload"
                      description={
                        s().kind === "traefik"
                          ? "Traefik watches its folder, so nothing needs to run."
                          : manual()
                            ? "The command to run on the server after copying the file."
                            : "kuroe runs it after writing. When it fails, the old file is put back."
                      }
                    >
                      <Input
                        mono
                        label="Test and reload"
                        disabled={props.disabled}
                        value={s().command}
                        onValue={(command) => patch({ command })}
                      />
                    </SettingRow>
                  }
                >
                  <SettingRow
                    type="stack"
                    label="Container"
                    description={`kuroe runs ${info().label}'s config test in it, then sends the reload signal.`}
                  >
                    <Input
                      mono
                      label="Container"
                      placeholder="nginx"
                      disabled={props.disabled}
                      value={s().container}
                      onValue={(container) => patch({ container })}
                    />
                  </SettingRow>
                </Show>
                <Show when={front() && !info().ownCertificates}>
                  <SettingRow
                    type="stack"
                    label="Certificate and key"
                    description={
                      <>
                        Where {info().label} finds each site's certificate. kuroe doesn't create these files. The
                        defaults are where certbot puts them. <span class="mono">{"{host}"}</span> becomes each
                        hostname.
                      </>
                    }
                  >
                    <FieldRow>
                      <Field label="Certificate" grow>
                        <Input
                          mono
                          label="Certificate"
                          disabled={props.disabled}
                          value={s().cert}
                          onValue={(cert) => patch({ cert })}
                        />
                      </Field>
                      <Field label="Key" grow>
                        <Input
                          mono
                          label="Key"
                          disabled={props.disabled}
                          value={s().key}
                          onValue={(key) => patch({ key })}
                        />
                      </Field>
                    </FieldRow>
                  </SettingRow>
                </Show>
                <Show when={!front()}>
                  <SettingRow
                    type="stack"
                    label="Your own HTTPS sites"
                    description="Port 443 now goes through the hostname check first. Your server's own HTTPS sites move to this address."
                  >
                    <Input
                      mono
                      label="Your own HTTPS sites"
                      disabled={props.disabled}
                      value={s().rest_tls}
                      onValue={(rest_tls) => patch({ rest_tls })}
                    />
                  </SettingRow>
                </Show>
                <Show when={!manual()}>
                  <SettingRow label="Keep it in sync" description="Rewrite the config and reload when domains change.">
                    <Switch checked={s().auto} disabled={props.disabled} onChange={(auto) => patch({ auto })} />
                  </SettingRow>
                </Show>
                <div class="card-b stack" style={{ gap: "12px" }}>
                  <Show when={props.error && props.saved && !manual()}>
                    <Note tone="bad">Last install failed: {props.error}</Note>
                  </Show>
                  <Show when={output()}>{(text) => <pre class="code">{text()}</pre>}</Show>
                  <div class="hrow wrap" style={{ gap: "8px" }}>
                    <Button
                      size="sm"
                      variant={manual() ? "primary" : undefined}
                      disabled={busy()}
                      onClick={showPreview}
                    >
                      <Icon name="code" />
                      Show config
                    </Button>
                    <Show when={!manual()}>
                      <Button
                        size="sm"
                        variant="primary"
                        disabled={busy() || props.disabled || !props.saved}
                        tooltip={props.saved ? undefined : "Save the settings first"}
                        onClick={install}
                      >
                        Install into {info().label}
                      </Button>
                    </Show>
                  </div>
                  <Show when={preview()}>
                    {(p) => (
                      <>
                        <Show when={p().skipped.length > 0}>
                          <Note tone="warn">
                            Left out because their port isn't published on the host: {p().skipped.join(", ")}
                          </Note>
                        </Show>
                        <ConfigFiles files={p().files} steps={p().steps} />
                      </>
                    )}
                  </Show>
                </div>
              </>
            );
          }}
        </Show>
      </Show>
    </Card>
  );
}
