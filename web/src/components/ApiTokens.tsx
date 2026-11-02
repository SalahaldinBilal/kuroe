import { createResource, createSignal, For, Show } from "solid-js";
import { API_BASE, type ApiToken, api } from "../api";
import { formatAgo, formatTime } from "../format";
import useToastState from "../states/toastState";
import { Card, Note } from "./ui/Blocks";
import Button from "./ui/Button/Button";
import EmptyState from "./ui/EmptyState/EmptyState";
import Field from "./ui/Field/Field";
import { Icon } from "./ui/Icon";
import Input from "./ui/Input/Input";
import Modal from "./ui/Modal/Modal";
import { CopyButton, ready, SecretField } from "./ui/Overlay";
import Select from "./ui/Select/Select";
import { Busy, SkeletonEmpty } from "./ui/Skeleton";

const EXPIRY = [
  { value: "30", label: "30 days" },
  { value: "90", label: "90 days" },
  { value: "365", label: "1 year" },
  { value: "never", label: "Never" },
];

interface Props {
  owner: boolean;
  publicUrl: string;
}

export function mcpUrl(publicUrl: string): string {
  return `${publicUrl.trim().replace(/\/+$/, "") || location.origin}${API_BASE}/mcp`;
}

export function claudeCommand(url: string, token: string): string {
  return `claude mcp add --transport http kuroe ${url} --header "Authorization: Bearer ${token}"`;
}

function expiry(t: ApiToken): string {
  if (!t.expires_at) return "Never";
  return t.expires_at * 1000 < Date.now() ? "Expired" : formatTime(t.expires_at);
}

export function ApiTokens(props: Props) {
  const { pushToast, pushError } = useToastState;
  const [tokens, { refetch }] = createResource(() => props.owner, api.apiTokens);
  const [creating, setCreating] = createSignal(false);
  const [name, setName] = createSignal("");
  const [days, setDays] = createSignal("90");
  const [value, setValue] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  const url = () => mcpUrl(props.publicUrl);

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

  const create = () =>
    run(async () => {
      const created = await api.createApiToken(name().trim(), days() === "never" ? null : Number(days()));
      setValue(created.value);
      refetch();
    });

  function close() {
    setCreating(false);
    setValue(null);
    setName("");
    setDays("90");
  }

  return (
    <Card
      title="API tokens"
      description="Read-only access for AI assistants and other MCP clients. A token sees what you can see and can't change anything."
      actions={
        <Button size="sm" onClick={() => setCreating(true)}>
          <Icon name="plus" />
          Create token
        </Button>
      }
    >
      <div class="card-b" style={{ "border-bottom": "1px solid var(--line-soft)" }}>
        <SecretField
          label="MCP server URL"
          value={url()}
          hint="Send the token as a bearer token. It works nowhere else in kuroe."
        />
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
            <EmptyState compact icon="key" title="No tokens" description="Create one to connect an MCP client." />
          </Show>
        }
      >
        <table class="tbl">
          <thead>
            <tr>
              <th>Name</th>
              <Show when={props.owner}>
                <th>User</th>
              </Show>
              <th class="hide-sm">Expires</th>
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
                  <Show when={props.owner}>
                    <td class="mono muted">{t.username}</td>
                  </Show>
                  <td class="hide-sm mono muted">{expiry(t)}</td>
                  <td class={t.last_used_at ? "muted" : "faint"}>
                    {t.last_used_at ? formatAgo(t.last_used_at) : "Never"}
                  </td>
                  <td class="acts">
                    <Button
                      variant="danger"
                      size="sm"
                      disabled={busy()}
                      onClick={() =>
                        run(async () => {
                          await api.revokeApiToken(t.id, props.owner);
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

      <Modal
        show={creating()}
        title={value() ? "Token created" : "Create API token"}
        onHide={close}
        footer={
          <Show
            when={!value()}
            fallback={
              <Button variant="primary" onClick={close}>
                Done
              </Button>
            }
          >
            <Button variant="ghost" onClick={close}>
              Cancel
            </Button>
            <Button variant="primary" disabled={busy() || !name().trim()} onClick={create}>
              Create token
            </Button>
          </Show>
        }
      >
        <Show
          when={value()}
          fallback={
            <div class="stack" style={{ gap: "14px" }}>
              <Field label="Name" hint="Only used to tell tokens apart.">
                <Input
                  autofocus
                  label="Token name"
                  maxLength={60}
                  placeholder="claude"
                  value={name()}
                  onValue={setName}
                />
              </Field>
              <Field label="Expires">
                <Select label="Expires" items={EXPIRY} value={days()} onChange={setDays} />
              </Field>
            </div>
          }
        >
          {(token) => (
            <div class="stack" style={{ gap: "14px" }}>
              <Note tone="warn">Copy it now. kuroe stores only a hash and can't show it again.</Note>
              <SecretField label="Token" value={token()} />
              <Field label="Add it to Claude Code">
                <div class="code-wrap">
                  <pre class="code" style={{ "white-space": "pre-wrap" }}>
                    {claudeCommand(url(), token())}
                  </pre>
                  <CopyButton text={claudeCommand(url(), token())} />
                </div>
              </Field>
              <span class="hint">
                Other MCP clients need the server URL and an Authorization header with the token.
              </span>
            </div>
          )}
        </Show>
      </Modal>
    </Card>
  );
}
