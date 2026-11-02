import { createSignal, For, Show } from "solid-js";
import { api, type ConfigFile, type ProxySettings, type WebServerKind, type WebServerPreview } from "../api";
import useToastState from "../states/toastState";
import { extraFile, KINDS, serverDefaults, switchKind, WEB_SERVERS, type WebServerDraft } from "../webserver";
import { Card, Note } from "./ui/Blocks";
import Button from "./ui/Button/Button";
import Field, { FieldRow } from "./ui/Field/Field";
import { Tiles } from "./ui/Form";
import { Icon } from "./ui/Icon";
import Input from "./ui/Input/Input";
import { CopyButton } from "./ui/Overlay";
import SegmentedControl from "./ui/SegmentedControl/SegmentedControl";
import Select from "./ui/Select/Select";
import Switch from "./ui/Switch/Switch";

/** Config files with their path and a copy button, plus the lines the user adds to their own config once. */
export function ConfigFiles(props: { files: ConfigFile[]; steps: string[] }) {
  return (
    <div class="stack" style={{ gap: "10px" }}>
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
            <span class="mono muted">{file.path}</span>
            <div class="code-wrap">
              <pre class="code">{file.body || "(empty)"}</pre>
              <CopyButton text={file.body} />
            </div>
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

export function WebServerCard(props: Props) {
  const { pushToast, pushError } = useToastState;
  const [preview, setPreview] = createSignal<WebServerPreview | null>(null);
  const [output, setOutput] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);

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
        <div class="card-b stack" style={{ gap: "14px" }}>
          <Field label="Web server">
            <Select
              label="Web server"
              disabled={props.disabled}
              items={kindItems()}
              value={props.server?.kind ?? OTHER}
              onChange={pick}
            />
          </Field>
          <Show
            when={props.server}
            fallback={
              <Note>
                Send kuroe's hostnames to <span class="mono">http://{props.behindHttp}</span>. Keep the Host header and
                set X-Forwarded-Proto. Your server handles HTTPS.
              </Note>
            }
          >
            {(s) => {
              const info = () => WEB_SERVERS[s().kind];
              const extra = () => extraFile(s().kind, props.behind ? s().mode : "front");
              const front = () => !props.behind || s().mode === "front";
              return (
                <>
                  <Show when={props.behind}>
                    <Field label="Who handles HTTPS">
                      <Show
                        when={info().passthrough}
                        fallback={
                          <span class="hint">
                            {info().label} can't pass HTTPS through untouched, so it handles HTTPS itself.
                          </span>
                        }
                      >
                        <Tiles
                          label="Who handles HTTPS"
                          columns={2}
                          value={s().mode}
                          disabled={props.disabled}
                          onChange={(mode) => patch({ mode })}
                          options={[
                            {
                              value: "front",
                              label: info().label,
                              icon: "server",
                              description: "It keeps its own certificates and forwards plain HTTP to Traefik.",
                            },
                            {
                              value: "passthrough",
                              label: "Traefik",
                              icon: "shield",
                              description: "It passes HTTPS through by hostname. Traefik gets the certificates.",
                            },
                          ]}
                        />
                      </Show>
                    </Field>
                  </Show>
                  <FieldRow>
                    <Field label="Config file" grow>
                      <Input
                        mono
                        label="Config file"
                        disabled={props.disabled}
                        value={s().path}
                        onValue={(path) => patch({ path })}
                      />
                    </Field>
                    <Show when={extra()}>
                      {(label) => (
                        <Field label={label()} grow>
                          <Input
                            mono
                            label={label()}
                            disabled={props.disabled}
                            value={s().extra_path}
                            onValue={(extra_path) => patch({ extra_path })}
                          />
                        </Field>
                      )}
                    </Show>
                  </FieldRow>
                  <Field label="Runs on">
                    <SegmentedControl
                      label="Runs on"
                      value={s().where}
                      onChange={(where) => patch({ where })}
                      options={
                        [
                          { value: "host", label: "This machine" },
                          { value: "container", label: "A container" },
                        ] as const
                      }
                    />
                  </Field>
                  <Show
                    when={s().where === "container"}
                    fallback={
                      <Field
                        label="Test and reload"
                        hint={
                          s().kind === "traefik"
                            ? "Traefik watches its folder, so nothing needs to run."
                            : "Runs after writing. When it fails, the old files are put back."
                        }
                      >
                        <Input
                          mono
                          label="Test and reload"
                          disabled={props.disabled}
                          value={s().command}
                          onValue={(command) => patch({ command })}
                        />
                      </Field>
                    }
                  >
                    <Field
                      label="Container"
                      hint="kuroe runs the config test in it, then sends the reload signal. Mount its config folder into kuroe at the same path."
                    >
                      <Input
                        mono
                        label="Container"
                        placeholder="nginx"
                        disabled={props.disabled}
                        value={s().container}
                        onValue={(container) => patch({ container })}
                      />
                    </Field>
                  </Show>
                  <Show when={front() && !info().ownCertificates}>
                    <FieldRow>
                      <Field label="Certificate file" grow hint="{host} is replaced by each hostname.">
                        <Input
                          mono
                          label="Certificate file"
                          disabled={props.disabled}
                          value={s().cert}
                          onValue={(cert) => patch({ cert })}
                        />
                      </Field>
                      <Field label="Key file" grow>
                        <Input
                          mono
                          label="Key file"
                          disabled={props.disabled}
                          value={s().key}
                          onValue={(key) => patch({ key })}
                        />
                      </Field>
                    </FieldRow>
                  </Show>
                  <Show when={!front()}>
                    <Field
                      label="Your own HTTPS sites"
                      hint="Port 443 now goes through the hostname check first. Your server's own HTTPS sites move to this address."
                    >
                      <Input
                        mono
                        label="Your own HTTPS sites"
                        disabled={props.disabled}
                        value={s().rest_tls}
                        onValue={(rest_tls) => patch({ rest_tls })}
                      />
                    </Field>
                  </Show>
                  <Switch
                    label="Rewrite and reload when domains change"
                    checked={s().auto}
                    disabled={props.disabled}
                    onChange={(auto) => patch({ auto })}
                  />
                  <Show when={props.error}>{(error) => <Note tone="bad">Last install failed: {error()}</Note>}</Show>
                  <Show when={output()}>{(text) => <pre class="code">{text()}</pre>}</Show>
                  <div class="hrow wrap" style={{ gap: "8px" }}>
                    <Button size="sm" disabled={busy()} onClick={showPreview}>
                      <Icon name="code" />
                      Preview
                    </Button>
                    <Button
                      size="sm"
                      variant="primary"
                      disabled={busy() || props.disabled || !props.saved}
                      tooltip={props.saved ? undefined : "Save the settings first"}
                      onClick={install}
                    >
                      Install into {info().label}
                    </Button>
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
                </>
              );
            }}
          </Show>
        </div>
      </Show>
    </Card>
  );
}
