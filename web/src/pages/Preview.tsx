import { A, useParams, useSearchParams } from "@solidjs/router";
import { createEffect, createResource, createSignal, For, Match, Show, Switch } from "solid-js";
import { type PreviewAnswer as Answer, api, type ContainerRow } from "../api";
import { PreviewAnswer } from "../components/PreviewAnswer";
import { Card, Note, PageHeader } from "../components/ui/Blocks";
import Button from "../components/ui/Button/Button";
import { PairRows } from "../components/ui/Editors";
import EmptyState, { Loading } from "../components/ui/EmptyState/EmptyState";
import { Icon } from "../components/ui/Icon";
import Input from "../components/ui/Input/Input";
import { ready } from "../components/ui/Overlay";
import SegmentedControl from "../components/ui/SegmentedControl/SegmentedControl";
import Select, { items } from "../components/ui/Select/Select";
import Spinner from "../components/ui/Spinner/Spinner";
import {
  addToHistory,
  type BodyMode,
  encodeBody,
  loadHistory,
  METHODS,
  outgoingHeaders,
  type Pair,
  type SavedRequest,
  saveHistory,
  statusTone,
  withQuery,
} from "../preview";

const CUSTOM = "custom";
const METHOD_ITEMS = items(METHODS);
type Section = "params" | "headers" | "body";

function validPort(text: string): number | null {
  const port = Number(text);
  return Number.isInteger(port) && port >= 1 && port <= 65535 ? port : null;
}

export function Preview() {
  const params = useParams<{ id: string }>();
  const [search] = useSearchParams<{ container?: string; port?: string }>();
  const [detail] = createResource(() => params.id, api.deployment);
  const running = () => (ready(detail)?.containers ?? []).filter((c) => c.state === "running");

  const [container, setContainer] = createSignal("");
  const [portChoice, setPortChoice] = createSignal("");
  const [customPort, setCustomPort] = createSignal("");
  const [method, setMethod] = createSignal("GET");
  const [path, setPath] = createSignal("/");
  const [query, setQuery] = createSignal<Pair[]>([]);
  const [headers, setHeaders] = createSignal<Pair[]>([]);
  const [mode, setMode] = createSignal<BodyMode>("none");
  const [text, setText] = createSignal("");
  const [rawType, setRawType] = createSignal("text/plain");
  const [form, setForm] = createSignal<Pair[]>([]);
  const [section, setSection] = createSignal<Section>("params");
  const [sending, setSending] = createSignal(false);
  const [answer, setAnswer] = createSignal<Answer | null>(null);
  const [sentTo, setSentTo] = createSignal({ container: "", port: 0, path: "/" });
  const [error, setError] = createSignal<string | null>(null);
  const [history, setHistory] = createSignal<SavedRequest[]>(loadHistory(params.id));

  const selected = (): ContainerRow | undefined => running().find((c) => c.id === container());
  const ports = () => selected()?.private_ports ?? [];
  const port = () => (portChoice() === CUSTOM ? validPort(customPort()) : validPort(portChoice()));

  /** Picks `wanted` when given: from the container's ports, else as a custom port. Otherwise its first port. */
  function pickPort(row: ContainerRow | undefined, wanted?: string) {
    const known = row?.private_ports ?? [];
    if (wanted && known.includes(Number(wanted))) {
      setPortChoice(wanted);
    } else if (wanted && validPort(wanted)) {
      setCustomPort(wanted);
      setPortChoice(CUSTOM);
    } else {
      setPortChoice(known.length ? String(known[0]) : CUSTOM);
    }
  }

  const nameOf = (id: string) => running().find((c) => c.id === id)?.name;

  createEffect(() => {
    const list = running();
    if (!list.length || list.some((c) => c.id === container())) return;
    const wanted = list.find((c) => c.id === search.container) ?? list.find((c) => c.private_ports.length) ?? list[0];
    setContainer(wanted.id);
    pickPort(wanted, wanted.id === search.container ? search.port : undefined);
  });

  function chooseContainer(id: string) {
    setContainer(id);
    pickPort(running().find((c) => c.id === id));
  }

  const portItems = () => [
    ...ports().map((p) => ({ value: String(p), label: String(p) })),
    { value: CUSTOM, label: "Custom port" },
  ];

  const fullPath = () => withQuery(path().trim() || "/", query());

  async function send() {
    const to = port();
    if (!container() || to === null) {
      setError("Pick a container and a port between 1 and 65535.");
      return;
    }
    const encoded = encodeBody(mode(), text(), rawType(), form());
    if (encoded.error) {
      setError(encoded.error);
      return;
    }
    setSending(true);
    setError(null);
    const target = { container: container(), port: to, path: fullPath() };
    try {
      const result = await api.sendPreviewRequest(container(), {
        port: to,
        method: method(),
        path: target.path,
        headers: outgoingHeaders(headers(), encoded.contentType),
        body: encoded.body,
      });
      setAnswer(result);
      setSentTo(target);
      remember(result.status);
    } catch (err) {
      setAnswer(null);
      setError(err instanceof Error ? err.message : String(err));
      remember(undefined);
    } finally {
      setSending(false);
    }
  }

  function remember(status: number | undefined) {
    const entry: SavedRequest = {
      container: container(),
      port: port() ?? 0,
      method: method(),
      path: path(),
      params: query(),
      headers: headers(),
      mode: mode(),
      text: text(),
      rawType: rawType(),
      form: form(),
      status,
      at: Date.now(),
    };
    const next = addToHistory(history(), entry);
    setHistory(next);
    saveHistory(params.id, next);
  }

  function restore(entry: SavedRequest) {
    if (running().some((c) => c.id === entry.container)) {
      setContainer(entry.container);
      pickPort(selected(), String(entry.port));
    }
    setMethod(entry.method);
    setPath(entry.path);
    setQuery(entry.params);
    setHeaders(entry.headers);
    setMode(entry.mode);
    setText(entry.text);
    setRawType(entry.rawType);
    setForm(entry.form);
  }

  const count = (rows: Pair[]) => rows.filter((r) => r.key.trim()).length;
  const tabUrl = () => (port() ? `/preview/${container()}/${port()}${fullPath()}` : undefined);

  return (
    <div class="content">
      <div class="crumbs">
        <A href="/deployments">Deployments</A>
        <Icon name="right" />
        <A href={`/deployments/${params.id}`}>{ready(detail)?.name ?? "Deployment"}</A>
        <Icon name="right" />
        <span>Preview</span>
      </div>
      <PageHeader
        title="Preview"
        description={`Send requests to the containers of ${ready(detail)?.name ?? "this deployment"}`}
      />
      <Show when={ready(detail)} fallback={<PreviewFallback error={detail.error} />}>
        {(d) => (
          <Switch>
            <Match when={!d().permissions.includes("dep.shell")}>
              <Card>
                <EmptyState
                  icon="lock"
                  title="You can't preview this deployment"
                  description="Previews need the permission to open shells, because they reach the containers directly."
                />
              </Card>
            </Match>
            <Match when={running().length === 0}>
              <Card>
                <EmptyState
                  icon="box"
                  title="Nothing is running"
                  description="Deploy or start the deployment to preview it."
                />
              </Card>
            </Match>
            <Match when={true}>
              <div class="preview-split">
                <div class="preview-left">
                  <Card
                    class="preview-req"
                    title="Request"
                    description="Sent from the server, straight to the container"
                  >
                    <div class="card-b stack" style={{ gap: "12px" }}>
                      <div class="hrow wrap">
                        <Select
                          class="grow"
                          label="Container"
                          items={running().map((c) => ({ value: c.id, label: c.name, hint: c.service ?? "app" }))}
                          value={container()}
                          onChange={chooseContainer}
                        />
                        <Select mono label="Port" items={portItems()} value={portChoice()} onChange={setPortChoice} />
                        <Show when={portChoice() === CUSTOM}>
                          <Input
                            mono
                            inputMode="numeric"
                            label="Custom port"
                            placeholder="8080"
                            style={{ width: "96px" }}
                            value={customPort()}
                            onValue={setCustomPort}
                          />
                        </Show>
                      </div>
                      <div class="hrow">
                        <Select mono label="Method" items={METHOD_ITEMS} value={method()} onChange={setMethod} />
                        <Input
                          mono
                          class="grow"
                          label="Path"
                          placeholder="/"
                          value={path()}
                          onValue={setPath}
                          onKeyDown={(e) => {
                            if (e.key === "Enter") send();
                          }}
                        />
                        <Button variant="primary" disabled={sending()} onClick={send}>
                          <Show when={sending()} fallback={<Icon name="play" />}>
                            <Spinner size={14} />
                          </Show>
                          Send
                        </Button>
                      </div>
                      <SegmentedControl
                        fill
                        label="Request part"
                        value={section()}
                        onChange={setSection}
                        options={[
                          { value: "params", label: "Query", count: count(query()) },
                          { value: "headers", label: "Headers", count: count(headers()) },
                          { value: "body", label: "Body" },
                        ]}
                      />
                      <Switch>
                        <Match when={section() === "params"}>
                          <PairRows
                            rows={query()}
                            keyLabel="Name"
                            valueLabel="Value"
                            addLabel="Add param"
                            onChange={setQuery}
                          />
                        </Match>
                        <Match when={section() === "headers"}>
                          <PairRows
                            rows={headers()}
                            keyLabel="Header"
                            valueLabel="Value"
                            addLabel="Add header"
                            onChange={setHeaders}
                          />
                        </Match>
                        <Match when={section() === "body"}>
                          <BodyEditor
                            mode={mode()}
                            text={text()}
                            rawType={rawType()}
                            form={form()}
                            onMode={setMode}
                            onText={setText}
                            onRawType={setRawType}
                            onForm={setForm}
                          />
                        </Match>
                      </Switch>
                    </div>
                    <div class="card-f">
                      <span class="hint grow mono">
                        {method()} {fullPath()}
                      </span>
                      <Button size="sm" href={tabUrl()} target="_blank" disabled={!tabUrl()}>
                        <Icon name="ext" />
                        Open in a new tab
                      </Button>
                    </div>
                  </Card>
                  <Show when={history().length > 0}>
                    <Card class="preview-hist" title="Recent requests" description="Kept in this browser tab">
                      <For each={history()}>
                        {(entry) => (
                          <button type="button" class="list-i" onClick={() => restore(entry)}>
                            <div class="nm grow">
                              <b class="mono">
                                {entry.method} {entry.path}
                              </b>
                              <span>
                                port {entry.port}
                                <Show when={nameOf(entry.container)}>{(name) => ` · ${name()}`}</Show>
                              </span>
                            </div>
                            <Show when={entry.status} fallback={<span class="muted">no answer</span>}>
                              {(status) => (
                                <span class={statusTone(status()) ? `badge s-${statusTone(status())}` : "badge"}>
                                  {status()}
                                </span>
                              )}
                            </Show>
                          </button>
                        )}
                      </For>
                    </Card>
                  </Show>
                </div>
                <Card class="preview-resp" title="Response">
                  <Show when={error()}>
                    <div class="card-b">
                      <Note tone="bad">{error()}</Note>
                    </div>
                  </Show>
                  <Show
                    when={answer()}
                    fallback={
                      <Show when={!error()}>
                        <EmptyState
                          icon="activity"
                          title="No request sent yet"
                          description="Send one to see the answer here."
                        />
                      </Show>
                    }
                  >
                    {(a) => <PreviewAnswer answer={a()} base={sentTo()} />}
                  </Show>
                </Card>
              </div>
            </Match>
          </Switch>
        )}
      </Show>
    </div>
  );
}

function PreviewFallback(props: { error: unknown }) {
  return (
    <Show
      when={props.error}
      fallback={
        <Card>
          <Loading />
        </Card>
      }
    >
      <Card>
        <EmptyState
          icon="alert"
          tone="danger"
          title="Couldn't load this deployment"
          description={String((props.error as Error | undefined)?.message ?? props.error)}
        />
      </Card>
    </Show>
  );
}

interface BodyProps {
  mode: BodyMode;
  text: string;
  rawType: string;
  form: Pair[];
  onMode: (mode: BodyMode) => void;
  onText: (text: string) => void;
  onRawType: (type: string) => void;
  onForm: (rows: Pair[]) => void;
}

function BodyEditor(props: BodyProps) {
  return (
    <div class="stack" style={{ gap: "10px" }}>
      <SegmentedControl
        label="Body"
        value={props.mode}
        onChange={props.onMode}
        options={[
          { value: "none", label: "None" },
          { value: "json", label: "JSON" },
          { value: "form", label: "Form" },
          { value: "raw", label: "Raw" },
        ]}
      />
      <Switch>
        <Match when={props.mode === "none"}>
          <span class="hint">No body is sent.</span>
        </Match>
        <Match when={props.mode === "form"}>
          <PairRows
            rows={props.form}
            keyLabel="Field"
            valueLabel="Value"
            addLabel="Add field"
            onChange={props.onForm}
          />
        </Match>
        <Match when={props.mode === "json" || props.mode === "raw"}>
          <Show when={props.mode === "raw"}>
            <Input mono label="Content type" placeholder="text/plain" value={props.rawType} onValue={props.onRawType} />
          </Show>
          <textarea
            class="ta mono"
            rows={10}
            aria-label="Body"
            placeholder={props.mode === "json" ? '{\n  "name": "kuroe"\n}' : ""}
            spellcheck={false}
            value={props.text}
            onInput={(e) => props.onText(e.currentTarget.value)}
          />
        </Match>
      </Switch>
    </div>
  );
}
