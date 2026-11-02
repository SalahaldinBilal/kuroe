import { createSignal, For, Show } from "solid-js";
import { api, type ImportChange, type ImportPlan, type ImportResult } from "../api";
import { HOOK_STEPS } from "../components/CiPanel";
import { Card, Note, PageHeader, Pill, Steps } from "../components/ui/Blocks";
import Button from "../components/ui/Button/Button";
import EmptyState from "../components/ui/EmptyState/EmptyState";
import Field from "../components/ui/Field/Field";
import { Icon } from "../components/ui/Icon";
import Input from "../components/ui/Input/Input";
import { SecretField } from "../components/ui/Overlay";
import useToastState, { errorMessage } from "../states/toastState";

type Step = 0 | 1 | 2;

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

const ACTION_LABEL: Record<ImportChange["action"], string> = {
  create: "new",
  update: "update",
  unchanged: "unchanged",
};
const ACTION_TONE: Record<ImportChange["action"], "ok" | "warn" | undefined> = {
  create: "ok",
  update: "warn",
  unchanged: undefined,
};

function Changes(props: { title: string; icon: "box" | "users" | "shield" | "key" | "play"; changes: ImportChange[] }) {
  return (
    <Show when={props.changes.length > 0}>
      <Card title={props.title}>
        <For each={props.changes}>
          {(c) => (
            <div class="list-i">
              <Icon name={props.icon} />
              <b class={`mono act-${c.action}`} style={{ "font-weight": "500" }}>
                {c.name}
              </b>
              <span class="muted grow mono" style={{ "font-size": "12px" }}>
                {c.fields.join(", ")}
              </span>
              <Pill tone={ACTION_TONE[c.action]}>{ACTION_LABEL[c.action]}</Pill>
            </div>
          )}
        </For>
      </Card>
    </Show>
  );
}

type Prompt = ImportPlan["secrets"][number];

function promptGroup(key: string): string {
  if (key.startsWith("@identity/")) return "Logins";
  if (key.startsWith("@user/")) return "Users";
  return key.split("/")[0];
}

function grouped(prompts: Prompt[]): [string, Prompt[]][] {
  const groups = new Map<string, Prompt[]>();
  for (const p of prompts) {
    const name = promptGroup(p.key);
    groups.set(name, [...(groups.get(name) ?? []), p]);
  }
  return [...groups.entries()];
}

function NewWebhook(props: { hook: ImportResult["webhooks"][number] }) {
  const { pushToast, pushError } = useToastState;
  const [installed, setInstalled] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const setup = () => props.hook.setup;
  async function install() {
    setBusy(true);
    try {
      await api.installWebhook(props.hook.deployment_id);
      setInstalled(true);
      pushToast(`Webhook added for ${props.hook.deployment}`);
    } catch (err) {
      pushError(err);
    } finally {
      setBusy(false);
    }
  }
  return (
    <div class="card-b stack" style={{ gap: "12px" }}>
      <b class="mono">{props.hook.deployment}</b>
      <Show when={!installed()} fallback={<Note tone="ok">Added at the provider with its own new secret.</Note>}>
        <Show
          when={setup()?.urls}
          fallback={<Note tone="warn">Set kuroe's public URL in Settings, then copy the URL from the CI tab.</Note>}
        >
          {(urls) => (
            <SecretField
              label="Payload URL"
              value={urls()[setup()?.provider ?? "github"]}
              hint={HOOK_STEPS[setup()?.provider ?? "github"]}
            />
          )}
        </Show>
        <SecretField label="Secret" value={props.hook.secret} />
        <Show when={setup()?.can_install}>
          <div class="hrow">
            <Button
              variant="primary"
              size="sm"
              disabled={busy()}
              tooltip="Uses the deployment's saved login to add the webhook at the provider"
              onClick={install}
            >
              Add it for me
            </Button>
          </div>
        </Show>
      </Show>
    </div>
  );
}

export function Import() {
  const [step, setStep] = createSignal<Step>(0);
  const [text, setText] = createSignal("");
  const [plan, setPlan] = createSignal<ImportPlan | null>(null);
  const [secrets, setSecrets] = createSignal<Record<string, string>>({});
  const [error, setError] = createSignal("");
  const [result, setResult] = createSignal<ImportResult | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [dragging, setDragging] = createSignal(false);
  let fileInput!: HTMLInputElement;

  async function run(action: () => Promise<unknown>) {
    setError("");
    setBusy(true);
    try {
      await action();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  async function onFile(file: File | undefined) {
    if (!file) return;
    setText(await file.text());
    setPlan(null);
  }

  const preview = () =>
    run(async () => {
      setPlan(await api.importPreview(text()));
      setSecrets({});
      setStep(1);
    });

  const apply = () =>
    run(async () => {
      setResult(await api.importApply(text(), secrets()));
      setStep(2);
    });

  function restart() {
    setText("");
    setPlan(null);
    setStep(0);
  }

  const missing = () => (plan()?.secrets ?? []).some((s) => !secrets()[s.key]);
  const nothing = () => {
    const p = plan();
    return (
      !!p &&
      p.deployments.every((d) => d.action === "unchanged") &&
      p.users.every((u) => u.action === "unchanged") &&
      p.roles.every((r) => r.action === "unchanged") &&
      p.identities.every((i) => i.action === "unchanged") &&
      p.volumes.length === 0 &&
      p.settings.length === 0 &&
      p.webhooks.length === 0 &&
      p.tokens.length === 0
    );
  };
  const done = () => result()?.deployments.length ?? 0;
  const count = (list: ImportChange[], action: ImportChange["action"]) =>
    list.filter((c) => c.action === action).length;

  return (
    <div class="content narrow">
      <PageHeader title="Import" description="Load a .kuroe.toml export from another kuroe" />
      <Steps labels={["Load file", "Review", "Done"]} step={step()} />

      <Show when={step() === 0}>
        <Card>
          <div class="card-b stack">
            <button
              type="button"
              class={dragging() ? "drop over" : "drop"}
              onClick={() => fileInput.click()}
              onDragOver={(e) => {
                e.preventDefault();
                setDragging(true);
              }}
              onDragLeave={() => setDragging(false)}
              onDrop={(e) => {
                e.preventDefault();
                setDragging(false);
                onFile(e.dataTransfer?.files[0]);
              }}
            >
              <Icon name="upload" />
              <b>Drop a .kuroe.toml export here</b>
              <span>or click to choose a file</span>
            </button>
            <input
              ref={fileInput}
              class="hidden"
              type="file"
              accept=".toml"
              onChange={(e) => onFile(e.currentTarget.files?.[0])}
            />
            <Field label="Or paste it">
              <textarea
                class="ta"
                rows={10}
                aria-label="Export file contents"
                placeholder={'[[deployment]]\nname = "shop-api"'}
                value={text()}
                onInput={(e) => {
                  setText(e.currentTarget.value);
                  setPlan(null);
                }}
              />
            </Field>
            <Show when={error()}>
              <Note tone="bad">{error()}</Note>
            </Show>
          </div>
          <div class="card-f">
            <span class="hint grow">Nothing changes until you apply, and nothing is deployed.</span>
            <Button variant="primary" disabled={busy() || !text().trim()} onClick={preview}>
              Review changes
            </Button>
          </div>
        </Card>
      </Show>

      <Show when={step() === 1 && plan()}>
        {(p) => (
          <div class="stack">
            <div class="hrow wrap" style={{ gap: "6px" }}>
              <Show when={count(p().deployments, "create")}>
                {(n) => <Pill tone="ok">{plural(n(), "new deployment")}</Pill>}
              </Show>
              <Show when={count(p().deployments, "update")}>
                {(n) => <Pill tone="warn">{plural(n(), "updated deployment")}</Pill>}
              </Show>
              <Show when={count(p().deployments, "unchanged")}>{(n) => <Pill>{n()} unchanged</Pill>}</Show>
              <Show when={count(p().roles, "create")}>{(n) => <Pill tone="ok">{plural(n(), "new role")}</Pill>}</Show>
              <Show when={count(p().users, "create")}>{(n) => <Pill tone="ok">{plural(n(), "new user")}</Pill>}</Show>
              <Show when={p().volumes.length}>{(n) => <Pill tone="ok">{plural(n(), "new volume")}</Pill>}</Show>
              <Show when={p().settings.length}>{(n) => <Pill tone="warn">{plural(n(), "setting")}</Pill>}</Show>
            </div>
            <Changes title="Deployments" icon="box" changes={p().deployments} />
            <Changes title="Roles" icon="shield" changes={p().roles} />
            <Changes title="Identities" icon="key" changes={p().identities} />
            <Changes title="Users" icon="users" changes={p().users} />
            <Changes title="Runner pools (start switched off)" icon="play" changes={p().runner_pools} />
            <Show when={p().volumes.length > 0}>
              <Card title="New shared volumes">
                <For each={p().volumes}>
                  {(v) => (
                    <div class="list-i">
                      <Icon name="drive" />
                      <b class="mono act-create grow" style={{ "font-weight": "500" }}>
                        {v}
                      </b>
                      <Pill tone="ok">new</Pill>
                    </div>
                  )}
                </For>
              </Card>
            </Show>
            <Show when={p().settings.length > 0}>
              <Card title="Settings">
                <For each={p().settings}>
                  {(s) => (
                    <div class="list-i">
                      <span class="mono grow">{s.key}</span>
                      <span class="diff">
                        <s>{s.from ?? "unset"}</s>
                        <Icon name="right" />
                        <b>{s.to}</b>
                      </span>
                    </div>
                  )}
                </For>
              </Card>
            </Show>
            <Show when={p().webhooks.length > 0 || p().tokens.length > 0}>
              <Card
                title="New values"
                description="These can't be moved between servers. The import makes new ones and shows them once at the end."
              >
                <For each={p().webhooks}>
                  {(name) => (
                    <div class="list-i">
                      <Icon name="git" />
                      <b class="mono act-create" style={{ "font-weight": "500" }}>
                        {name}
                      </b>
                      <span class="muted grow">push webhook secret</span>
                      <Pill tone="ok">new</Pill>
                    </div>
                  )}
                </For>
                <For each={p().tokens}>
                  {(t) => (
                    <div class="list-i">
                      <Icon name="key" />
                      <b class="mono act-create" style={{ "font-weight": "500" }}>
                        {t.deployment}
                      </b>
                      <span class="muted grow">deploy token "{t.name}"</span>
                      <Pill tone="ok">new</Pill>
                    </div>
                  )}
                </For>
              </Card>
            </Show>
            <Card
              title={p().secrets.length > 0 ? "Secrets" : "Apply"}
              description={
                p().secrets.length > 0 ? "These were left out of the export. Enter them to finish." : undefined
              }
            >
              <Show when={p().secrets.length > 0}>
                <div class="card-b stack" style={{ gap: "18px" }}>
                  <For each={grouped(p().secrets)}>
                    {([group, prompts]) => (
                      <div class="stack" style={{ gap: "10px" }}>
                        <b class="mono">{group}</b>
                        <For each={prompts}>
                          {(s) => (
                            <Field label={s.label}>
                              <Input
                                type="password"
                                label={s.label}
                                value={secrets()[s.key] ?? ""}
                                onValue={(value) => setSecrets({ ...secrets(), [s.key]: value })}
                              />
                            </Field>
                          )}
                        </For>
                      </div>
                    )}
                  </For>
                </div>
              </Show>
              <Show when={error()}>
                <div class="card-b">
                  <Note tone="bad">{error()}</Note>
                </div>
              </Show>
              <div class="card-f">
                <Button variant="ghost" onClick={() => setStep(0)}>
                  <Icon name="back" />
                  Back
                </Button>
                <span class="grow" />
                <Show when={!nothing()} fallback={<span class="hint">Everything in this file is already here.</span>}>
                  <Button variant="primary" disabled={busy() || missing()} onClick={apply}>
                    Apply import
                  </Button>
                </Show>
              </div>
            </Card>
          </div>
        )}
      </Show>

      <Show when={step() === 2}>
        <div class="stack">
          <Card>
            <EmptyState
              icon="check"
              tone="success"
              title={`Imported ${done()} deployment${done() === 1 ? "" : "s"}`}
              description="They are created but not deployed. Deploy each one from its page when you're ready."
              actions={
                <>
                  <Button variant="primary" href="/deployments">
                    Go to deployments
                  </Button>
                  <Button onClick={restart}>Import another</Button>
                </>
              }
            />
          </Card>
          <Show when={result()?.webhooks.length}>
            <Card
              title="New push webhooks"
              description="The old webhooks point at the old server. Add these at the provider. The secrets are shown once."
            >
              <For each={result()?.webhooks}>{(hook) => <NewWebhook hook={hook} />}</For>
            </Card>
          </Show>
          <Show when={result()?.tokens.length}>
            <Card title="New deploy tokens" description="Update these in your CI. They're shown once.">
              <div class="card-b stack" style={{ gap: "12px" }}>
                <For each={result()?.tokens}>
                  {(t) => <SecretField label={`${t.deployment}: ${t.name}`} value={t.token} />}
                </For>
              </div>
            </Card>
          </Show>
        </div>
      </Show>
    </div>
  );
}
