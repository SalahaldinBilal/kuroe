import { createResource, createSignal, For, Show } from "solid-js";
import {
  API_BASE,
  api,
  type CiPlatform,
  type DeploymentDetail,
  type HookProvider,
  type HookUrls,
  type Pipeline,
} from "../api";
import { formatAgo, formatTime } from "../format";
import useToastState from "../states/toastState";
import { Card, Note, Pill } from "./ui/Blocks";
import Button from "./ui/Button/Button";
import EmptyState from "./ui/EmptyState/EmptyState";
import Field from "./ui/Field/Field";
import { Icon } from "./ui/Icon";
import Input from "./ui/Input/Input";
import Modal from "./ui/Modal/Modal";
import { CopyButton, ready, SecretField } from "./ui/Overlay";
import SegmentedControl from "./ui/SegmentedControl/SegmentedControl";
import { Busy, SkeletonEmpty } from "./ui/Skeleton";

interface Props {
  detail: DeploymentDetail;
}

const PLATFORMS: { value: CiPlatform; label: string }[] = [
  { value: "github", label: "GitHub" },
  { value: "gitlab", label: "GitLab" },
  { value: "gitea", label: "Gitea" },
  { value: "forgejo", label: "Forgejo" },
  { value: "shell", label: "Shell" },
];

export const EXPORT_HINT =
  "Secret values stay out unless you include them. Webhook secrets, deploy tokens and login secrets always stay out. The top of the file lists what's missing, and the Import page asks for it or makes new ones.";

export const HOOK_STEPS: Record<HookProvider, string> = {
  github: "In GitHub: Settings, Webhooks, Add webhook. Content type application/json, push events only.",
  gitlab: "In GitLab: Settings, Webhooks, Add new webhook. Paste the secret as the Secret token, push events only.",
  gitea: "In Gitea or Forgejo: Settings, Webhooks, Add webhook, Gitea. POST, application/json, push events only.",
};

/** The platform to show first: from the image registry or the repository host. */
function guessPlatform(detail: DeploymentDetail): CiPlatform {
  const s = detail.spec.source;
  const where = s.kind === "image" ? s.image : s.kind === "git" ? s.url : "";
  if (where.includes("gitlab")) return "gitlab";
  if (where.includes("codeberg") || where.includes("forgejo")) return "forgejo";
  if (s.kind === "git" && !where.includes("github")) return "gitea";
  return "github";
}

export function CiPanel(props: Props) {
  const { pushToast, pushError } = useToastState;
  const id = () => props.detail.id;
  const kind = () => props.detail.spec.source.kind;
  const [tokens, { refetch }] = createResource(id, api.tokens);
  const [webhook, { refetch: refetchHook }] = createResource(
    () => (kind() === "git" ? id() : null),
    (d) => api.webhook(d),
  );
  const [creating, setCreating] = createSignal(false);
  const [tokenName, setTokenName] = createSignal("");
  const [newToken, setNewToken] = createSignal<string | null>(null);
  const [newHook, setNewHook] = createSignal<{ urls: HookUrls; secret: string } | null>(null);
  const [platform, setPlatform] = createSignal<CiPlatform>(guessPlatform(props.detail));
  const [runOn, setRunOn] = createSignal<"hosted" | "here">("hosted");
  const [pipeline, setPipeline] = createSignal<Pipeline | null>(null);
  const [busy, setBusy] = createSignal(false);
  const provider = (): HookProvider => ready(webhook)?.provider ?? "github";
  const pipelineDescription = () =>
    kind() === "image"
      ? "Builds the image, pushes it tagged with the commit, and deploys that tag here"
      : kind() === "static"
        ? "Sends the source to this site on every push. kuroe builds and serves it."
        : "Starts a deploy on every push. kuroe pulls and builds the repository itself.";

  async function run(action: () => Promise<unknown>) {
    setBusy(true);
    try {
      await action();
    } catch (err) {
      pushError(err);
    } finally {
      setBusy(false);
    }
  }

  const createToken = () =>
    run(async () => {
      const created = await api.createToken(id(), tokenName().trim());
      setNewToken(created.secret);
      refetch();
    });

  function closeTokenDialog() {
    setCreating(false);
    setNewToken(null);
    setTokenName("");
  }

  const branch = () => {
    const s = props.detail.spec.source;
    return s.kind === "git" && s.reference ? s.reference : "the default branch";
  };
  const curl = () =>
    props.detail.spec.source.kind === "static"
      ? `tar -czf site.tar.gz -C dist .\ncurl -X PUT "${location.origin}${API_BASE}/deployments/${id()}/site?deploy=true" \\\n  -H "Authorization: Bearer $KUROE_TOKEN" \\\n  --data-binary @site.tar.gz`
      : `curl -X POST ${location.origin}${API_BASE}/deployments/${id()}/deploy \\\n  -H "Authorization: Bearer $KUROE_TOKEN" \\\n  -H "content-type: application/json" \\\n  -d '{"tag": "v1.2"}'`;

  return (
    <div class="stack">
      <Card
        title="Deploy tokens"
        description="A token can only deploy this deployment"
        actions={
          <Button size="sm" onClick={() => setCreating(true)}>
            <Icon name="plus" />
            Create token
          </Button>
        }
      >
        <div class="card-b" style={{ "border-bottom": "1px solid var(--line-soft)" }}>
          <div class="code-wrap">
            <pre class="code" style={{ "white-space": "pre-wrap" }}>
              {curl()}
            </pre>
            <CopyButton text={curl()} />
          </div>
          <p class="hint" style={{ "margin-top": "8px" }}>
            The tag is optional.
          </p>
        </div>
        <Show
          when={(ready(tokens) ?? []).length > 0}
          fallback={
            <Show
              when={tokens.latest || tokens.error}
              fallback={
                <Busy>
                  <SkeletonEmpty compact />
                </Busy>
              }
            >
              <EmptyState compact icon="key" title="No tokens" description="Create one for CI or a deploy script." />
            </Show>
          }
        >
          <table class="tbl">
            <thead>
              <tr>
                <th>Name</th>
                <th class="hide-sm">Created</th>
                <th>Last used</th>
                <th />
              </tr>
            </thead>
            <tbody>
              <For each={ready(tokens)}>
                {(t) => (
                  <tr>
                    <td>
                      <span class="hrow">
                        <Icon name="key" />
                        <b style={{ "font-weight": "500", color: "var(--ink)" }}>{t.name}</b>
                      </span>
                    </td>
                    <td class="hide-sm mono muted">{formatTime(t.created_at)}</td>
                    <td class={t.last_used_at ? "muted" : "faint"}>
                      {t.last_used_at ? formatAgo(t.last_used_at) : "Never"}
                    </td>
                    <td class="acts">
                      <Button
                        variant="danger"
                        size="sm"
                        onClick={() =>
                          run(async () => {
                            await api.deleteToken(id(), t.id);
                            pushToast(`Revoked ${t.name}`);
                            refetch();
                          })
                        }
                      >
                        Revoke
                      </Button>
                    </td>
                  </tr>
                )}
              </For>
            </tbody>
          </table>
        </Show>
      </Card>

      <Show when={kind() === "image" || kind() === "git" || kind() === "static"}>
        <Card title="CI pipeline" description={pipelineDescription()}>
          <div class="card-b stack" style={{ gap: "12px" }}>
            <div class="hrow wrap" style={{ gap: "8px" }}>
              <SegmentedControl
                label="Platform"
                value={platform()}
                onChange={(p) => {
                  setPlatform(p);
                  setPipeline(null);
                }}
                options={PLATFORMS}
              />
              <Show when={platform() !== "shell"}>
                <SegmentedControl
                  label="Runs on"
                  value={runOn()}
                  onChange={(r) => {
                    setRunOn(r);
                    setPipeline(null);
                  }}
                  options={
                    [
                      { value: "hosted", label: "Their runners" },
                      { value: "here", label: "This server" },
                    ] as const
                  }
                />
              </Show>
              <span class="grow" />
              <Button
                size="sm"
                disabled={busy()}
                onClick={() =>
                  pipeline()
                    ? setPipeline(null)
                    : run(async () => setPipeline(await api.ciConfig(id(), platform(), runOn())))
                }
              >
                {pipeline() ? "Hide" : "Show pipeline"}
              </Button>
            </div>
            <Show when={pipeline()}>
              {(p) => (
                <>
                  <Show when={p().path}>
                    <div class="hrow wrap" style={{ gap: "6px" }}>
                      <span class="muted">Save it as</span>
                      <span class="chip">{p().path}</span>
                    </div>
                  </Show>
                  <For each={p().notes}>{(note) => <span class="hint">{note}</span>}</For>
                  <div class="code-wrap">
                    <pre class="code">{p().body}</pre>
                    <CopyButton text={p().body} />
                  </div>
                </>
              )}
            </Show>
          </div>
        </Card>
      </Show>

      <Show when={kind() === "git"}>
        <Card
          title="Push webhook"
          description={`${provider() === "github" ? "GitHub" : provider() === "gitlab" ? "GitLab" : "Gitea or Forgejo"} calls this on every push. A push to ${branch()} pulls and builds a new revision.`}
          actions={
            <Pill tone={ready(webhook)?.enabled ? "ok" : undefined} dot>
              {ready(webhook)?.enabled ? "Enabled" : "Off"}
            </Pill>
          }
        >
          <Show
            when={!ready(webhook)?.app}
            fallback={
              <div class="card-b">
                <Note tone="ok">
                  Pushes come in through the GitHub App {ready(webhook)?.app}. There's nothing to set up here.
                </Note>
              </div>
            }
          >
            <div class="card-b stack" style={{ gap: "12px" }}>
              <Show when={newHook()}>
                {(hook) => (
                  <>
                    <Note tone="warn">Copy these now. The secret is shown once.</Note>
                    <SecretField label="Payload URL" value={hook().urls[provider()]} hint={HOOK_STEPS[provider()]} />
                    <SecretField label="Secret" value={hook().secret} />
                  </>
                )}
              </Show>
              <div class="hrow">
                <Button
                  size="sm"
                  disabled={busy()}
                  onClick={() =>
                    run(async () => {
                      setNewHook(await api.rotateWebhook(id()));
                      refetchHook();
                    })
                  }
                >
                  {ready(webhook)?.enabled ? "New secret" : "Enable"}
                </Button>
                <Show when={ready(webhook)?.can_install}>
                  <Button
                    size="sm"
                    variant="primary"
                    disabled={busy()}
                    tooltip="Uses this deployment's saved login to add the webhook at the provider"
                    onClick={() =>
                      run(async () => {
                        await api.installWebhook(id());
                        setNewHook(null);
                        pushToast("Webhook added at the provider");
                        refetchHook();
                      })
                    }
                  >
                    Add it for me
                  </Button>
                </Show>
                <Show when={ready(webhook)?.enabled}>
                  <Button
                    variant="danger"
                    size="sm"
                    disabled={busy()}
                    onClick={() =>
                      run(async () => {
                        await api.disableWebhook(id());
                        setNewHook(null);
                        pushToast("Webhook disabled");
                        refetchHook();
                      })
                    }
                  >
                    Disable
                  </Button>
                </Show>
              </div>
            </div>
          </Show>
        </Card>
      </Show>

      <Show when={props.detail.permissions.includes("dep.edit")}>
        <Card title="Export" description="A TOML file that recreates this deployment on another kuroe">
          <div class="card-b hrow wrap">
            <Button download href={api.exportUrl(id(), false)}>
              <Icon name="download" />
              Download
            </Button>
            <Show when={props.detail.permissions.includes("dep.secrets")}>
              <Button download href={api.exportUrl(id(), true)}>
                <Icon name="lock" />
                Download with secrets
              </Button>
            </Show>
            <span class="hint">{EXPORT_HINT}</span>
          </div>
        </Card>
      </Show>

      <Modal
        show={creating()}
        title={newToken() ? "Token created" : "Create token"}
        onHide={closeTokenDialog}
        footer={
          <Show
            when={!newToken()}
            fallback={
              <Button variant="primary" onClick={closeTokenDialog}>
                Done
              </Button>
            }
          >
            <Button variant="ghost" onClick={closeTokenDialog}>
              Cancel
            </Button>
            <Button variant="primary" disabled={busy() || !tokenName().trim()} onClick={createToken}>
              Create token
            </Button>
          </Show>
        }
      >
        <Show
          when={newToken()}
          fallback={
            <Field label="Name" hint="Only used to tell tokens apart.">
              <Input
                autofocus
                label="Token name"
                maxLength={64}
                placeholder="github-actions"
                value={tokenName()}
                onValue={setTokenName}
              />
            </Field>
          }
        >
          {(secret) => (
            <>
              <Note tone="warn">Copy it now. kuroe stores only a hash and can't show it again.</Note>
              <SecretField label="Token" value={secret()} />
            </>
          )}
        </Show>
      </Modal>
    </div>
  );
}
