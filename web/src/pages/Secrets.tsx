import { A } from "@solidjs/router";
import { createSignal, For, onMount, Show } from "solid-js";
import { useUser } from "../App";
import { api, can, type SavedSecret } from "../api";
import { WhitespaceNote } from "../components/SecretValue";
import { Card, PageHeader } from "../components/ui/Blocks";
import Button from "../components/ui/Button/Button";
import EmptyState from "../components/ui/EmptyState/EmptyState";
import Field from "../components/ui/Field/Field";
import { Icon } from "../components/ui/Icon";
import Input from "../components/ui/Input/Input";
import Modal from "../components/ui/Modal/Modal";
import { ready, SecretField } from "../components/ui/Overlay";
import { Busy, SkeletonEmpty, SkeletonList } from "../components/ui/Skeleton";
import Tip from "../components/ui/Tooltip/Tip";
import { formatAgo } from "../format";
import { rowsFor, useShape } from "../shape";
import savedSecretState from "../states/savedSecretState";
import useToastState from "../states/toastState";

interface Draft {
  id: string | null;
  name: string;
  description: string;
  value: string;
}

export function Secrets() {
  const user = useUser();
  const { pushToast, pushError } = useToastState;
  const canManage = () => can(user(), "secret.manage");
  const [editing, setEditing] = createSignal<Draft | null>(null);
  const [removing, setRemoving] = createSignal<SavedSecret | null>(null);
  const [revealed, setRevealed] = createSignal<{ name: string; value: string } | null>(null);
  const [busy, setBusy] = createSignal(false);
  const list = () => ready(savedSecretState.list);
  const known = useShape("secrets", 2, () => savedSecretState.list.latest?.length);
  const editedSecret = () => list()?.find((s) => s.id === editing()?.id);
  function usersOfEdited() {
    const names = (editedSecret()?.used_by ?? []).map((u) => u.name);
    return names.length > 0 ? names.join(", ") : "the deployments that use it";
  }

  onMount(() => {
    savedSecretState.load();
    savedSecretState.refetch();
  });

  async function save() {
    const d = editing();
    if (!d) return;
    setBusy(true);
    try {
      const body = { name: d.name.trim(), description: d.description.trim(), value: d.value };
      if (d.id) await api.updateSavedSecret(d.id, body);
      else await api.createSavedSecret(body);
      pushToast(d.id ? `Saved ${body.name}` : `Added ${body.name}`);
      setEditing(null);
      savedSecretState.refetch();
    } catch (err) {
      pushError(err);
    } finally {
      setBusy(false);
    }
  }

  async function remove(secret: SavedSecret) {
    try {
      await api.deleteSavedSecret(secret.id);
      pushToast(`Removed ${secret.name}`);
      setRemoving(null);
      savedSecretState.refetch();
    } catch (err) {
      pushError(err);
    }
  }

  async function reveal(secret: SavedSecret) {
    try {
      const { value } = await api.revealSavedSecret(secret.id);
      setRevealed({ name: secret.name, value });
    } catch (err) {
      pushError(err);
    }
  }

  return (
    <div class="content narrow">
      <PageHeader
        title="Secrets"
        description="Named values that deployments point at instead of holding their own copy. Change one here and every deployment gets it on its next deploy."
        actions={
          <Show when={canManage()}>
            <Button variant="primary" onClick={() => setEditing({ id: null, name: "", description: "", value: "" })}>
              <Icon name="plus" />
              Add secret
            </Button>
          </Show>
        }
      />
      <Show
        when={list()}
        fallback={
          <Show
            when={savedSecretState.list.error}
            fallback={
              <Busy>
                <Show
                  when={known > 0}
                  fallback={
                    <section class="card">
                      <SkeletonEmpty action={canManage()} />
                    </section>
                  }
                >
                  <SkeletonList head={false} rows={rowsFor(known)} actions={canManage() ? 3 : 0} />
                </Show>
              </Busy>
            }
          >
            <Card>
              <EmptyState
                icon="alert"
                tone="danger"
                title="Couldn't load saved secrets"
                description={String(savedSecretState.list.error?.message)}
              />
            </Card>
          </Show>
        }
      >
        {(secrets) => (
          <Card>
            <Show
              when={secrets().length > 0}
              fallback={
                <EmptyState
                  icon="lock"
                  title="No saved secrets yet"
                  description="Save a value once, then pick it on any deployment's environment or git token."
                  actions={
                    <Show when={canManage()}>
                      <Button
                        variant="primary"
                        onClick={() => setEditing({ id: null, name: "", description: "", value: "" })}
                      >
                        <Icon name="plus" />
                        Add secret
                      </Button>
                    </Show>
                  }
                />
              }
            >
              <For each={secrets()}>
                {(s) => (
                  <div class="list-i">
                    <span class="secret-icon">
                      <Icon name="key" />
                    </span>
                    <div class="nm grow">
                      <b class="mono">{s.name}</b>
                      <span class="plain">
                        {s.description || "No description"} · changed {formatAgo(s.updated_at)}
                      </span>
                    </div>
                    <span class="hrow wrap hide-sm" style={{ gap: "4px" }}>
                      <For each={s.used_by} fallback={<span class="faint">Not used</span>}>
                        {(u) => (
                          <A class="chip" href={`/deployments/${u.id}`}>
                            {u.name}
                          </A>
                        )}
                      </For>
                    </span>
                    <Show when={canManage()}>
                      <Button size="sm" onClick={() => reveal(s)}>
                        Reveal
                      </Button>
                      <Button
                        size="sm"
                        onClick={() => setEditing({ id: s.id, name: s.name, description: s.description, value: "" })}
                      >
                        Edit
                      </Button>
                      <Tip content={s.used_by.length > 0 ? "Used by deployments, switch them first" : "Remove"}>
                        <span>
                          <Button
                            variant="ghost"
                            icon
                            size="sm"
                            disabled={s.used_by.length > 0}
                            onClick={() => setRemoving(s)}
                          >
                            <Icon name="trash" />
                          </Button>
                        </span>
                      </Tip>
                    </Show>
                  </div>
                )}
              </For>
            </Show>
          </Card>
        )}
      </Show>

      <Modal
        show={editing() !== null}
        title={editing()?.id ? `Edit ${editedSecret()?.name ?? "secret"}` : "Add a saved secret"}
        onHide={() => setEditing(null)}
        footer={
          <>
            <Button variant="ghost" onClick={() => setEditing(null)}>
              Cancel
            </Button>
            <Button
              variant="primary"
              disabled={busy() || !editing()?.name.trim() || (!editing()?.id && !editing()?.value)}
              onClick={save}
            >
              {editing()?.id ? "Save" : "Add secret"}
            </Button>
          </>
        }
      >
        <Show when={editing()}>
          {(d) => (
            <>
              <Field label="Name" hint="Lowercase letters, digits and dashes. Deployments show this name.">
                <Input
                  mono
                  autofocus
                  label="Name"
                  placeholder="db-password"
                  value={d().name}
                  onValue={(name) => setEditing({ ...d(), name })}
                />
              </Field>
              <Field label="Description">
                <Input
                  label="Description"
                  placeholder="What it's for"
                  value={d().description}
                  onValue={(description) => setEditing({ ...d(), description })}
                />
              </Field>
              <Field
                label="Value"
                hint={
                  d().id
                    ? `Empty keeps the current value. A new value takes effect on the next deploy of ${usersOfEdited()}.`
                    : undefined
                }
              >
                <div class="secret-value">
                  <Input
                    mono
                    type="password"
                    label="Value"
                    placeholder={d().id ? "unchanged" : "value"}
                    value={d().value}
                    onValue={(value) => setEditing({ ...d(), value })}
                  />
                  <WhitespaceNote value={d().value} onTrim={(value) => setEditing({ ...d(), value })} />
                </div>
              </Field>
            </>
          )}
        </Show>
      </Modal>

      <Modal
        show={removing() !== null}
        title={`Remove ${removing()?.name}?`}
        onHide={() => setRemoving(null)}
        footer={
          <>
            <Button variant="ghost" onClick={() => setRemoving(null)}>
              Cancel
            </Button>
            <Button variant="danger-solid" onClick={() => removing() && remove(removing() as SavedSecret)}>
              Remove
            </Button>
          </>
        }
      >
        <p>kuroe forgets the value. Nothing uses it right now.</p>
      </Modal>

      <Modal show={revealed() !== null} title={revealed()?.name} onHide={() => setRevealed(null)}>
        <Show when={revealed()}>{(r) => <SecretField label="Value" value={r().value} />}</Show>
      </Modal>
    </div>
  );
}
