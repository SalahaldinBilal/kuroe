import { A } from "@solidjs/router";
import { createResource, createSignal, For, Show } from "solid-js";
import { useUser } from "../App";
import { api, can, type Volume } from "../api";
import { Card, Note, PageHeader } from "../components/ui/Blocks";
import Button from "../components/ui/Button/Button";
import EmptyState from "../components/ui/EmptyState/EmptyState";
import Field from "../components/ui/Field/Field";
import { Icon } from "../components/ui/Icon";
import Input from "../components/ui/Input/Input";
import Modal from "../components/ui/Modal/Modal";
import { ready } from "../components/ui/Overlay";
import { Busy, SkeletonEmpty, SkeletonTable } from "../components/ui/Skeleton";
import { formatTime } from "../format";
import { rowsFor, useShape } from "../shape";
import useToastState from "../states/toastState";

export function Volumes() {
  const user = useUser();
  const { pushToast, pushError } = useToastState;
  const [volumes, { refetch }] = createResource(api.volumes);
  const known = useShape("volumes", 2, () => volumes.latest?.length);
  const [deployments] = createResource(api.deployments);
  const [creating, setCreating] = createSignal(false);
  const [name, setName] = createSignal("");
  const [deleting, setDeleting] = createSignal<Volume | null>(null);
  const [busy, setBusy] = createSignal(false);
  const canCreate = () => can(user(), "vol") || can(user(), "dep.create");
  const canDelete = () => can(user(), "vol");

  const mountedBy = (volume: string) =>
    (ready(deployments) ?? []).filter((d) =>
      Object.values(d.spec.services).some((s) => s.volumes.some((m) => m.volume === volume)),
    );

  async function run(action: () => Promise<unknown>, done: string) {
    setBusy(true);
    try {
      await action();
      pushToast(done);
      refetch();
      return true;
    } catch (err) {
      pushError(err);
      return false;
    } finally {
      setBusy(false);
    }
  }

  async function create() {
    const n = name().trim();
    if (await run(() => api.createVolume(n), `Volume ${n} created`)) {
      setCreating(false);
      setName("");
    }
  }

  async function remove(v: Volume) {
    if (await run(() => api.removeVolume(v.name), `Volume ${v.name} deleted`)) setDeleting(null);
  }

  const newButton = (small: boolean) => (
    <Show when={canCreate()}>
      <Button variant="primary" size={small ? "sm" : "md"} onClick={() => setCreating(true)}>
        <Icon name="plus" />
        New volume
      </Button>
    </Show>
  );

  return (
    <div class="content">
      <PageHeader
        title="Volumes"
        description="Named volumes you can mount into any service of any deployment"
        actions={newButton(false)}
      />
      <Show when={volumes.error}>
        <Note tone="bad">{String(volumes.error?.message)}</Note>
      </Show>
      <Card>
        <Show
          when={(ready(volumes) ?? []).length > 0}
          fallback={
            <Show
              when={volumes.latest || volumes.error}
              fallback={
                <Busy>
                  <Show when={known > 0} fallback={<SkeletonEmpty action={canCreate()} />}>
                    <SkeletonTable bare rows={rowsFor(known)} columns={5} />
                  </Show>
                </Busy>
              }
            >
              <EmptyState
                icon="drive"
                title="No shared volumes"
                description="Create one to share files between deployments."
                actions={newButton(true)}
              />
            </Show>
          }
        >
          <table class="tbl">
            <thead>
              <tr>
                <th>Name</th>
                <th class="hide-sm">Docker name</th>
                <th>Mounted by</th>
                <th class="hide-sm">Created</th>
                <th />
              </tr>
            </thead>
            <tbody>
              <For each={ready(volumes)}>
                {(v) => (
                  <tr>
                    <td>
                      <span class="hrow">
                        <Icon name="drive" />
                        <b class="mono" style={{ "font-weight": "500", color: "var(--ink)" }}>
                          {v.name}
                        </b>
                      </span>
                    </td>
                    <td class="hide-sm mono muted">kuroe_{v.name}</td>
                    <td>
                      <Show when={mountedBy(v.name).length > 0} fallback={<span class="faint">Not mounted</span>}>
                        <span class="hrow wrap" style={{ gap: "4px" }}>
                          <For each={mountedBy(v.name)}>
                            {(d) => (
                              <A class="chip" href={`/deployments/${d.id}`}>
                                {d.name}
                              </A>
                            )}
                          </For>
                        </span>
                      </Show>
                    </td>
                    <td class="hide-sm mono muted">{formatTime(v.created_at)}</td>
                    <td class="acts">
                      <Show when={canDelete()}>
                        <Button variant="ghost" icon size="sm" tooltip="Delete" onClick={() => setDeleting(v)}>
                          <Icon name="trash" />
                        </Button>
                      </Show>
                    </td>
                  </tr>
                )}
              </For>
            </tbody>
          </table>
        </Show>
      </Card>

      <Modal
        show={creating()}
        title="New volume"
        onHide={() => setCreating(false)}
        footer={
          <>
            <Button variant="ghost" onClick={() => setCreating(false)}>
              Cancel
            </Button>
            <Button type="submit" form="new-volume" variant="primary" disabled={busy()}>
              Create volume
            </Button>
          </>
        }
      >
        <form
          id="new-volume"
          onSubmit={(e) => {
            e.preventDefault();
            create();
          }}
        >
          <Field label="Name" hint="Docker names it kuroe_<name>. Lowercase letters, numbers and dashes.">
            <Input
              mono
              autofocus
              required
              label="Volume name"
              pattern="[a-z0-9][a-z0-9\-]{0,39}"
              placeholder="uploads"
              value={name()}
              onValue={setName}
            />
          </Field>
        </form>
      </Modal>

      <Modal
        show={deleting() != null}
        title={`Delete ${deleting()?.name}?`}
        onHide={() => setDeleting(null)}
        footer={
          <>
            <Button variant="ghost" onClick={() => setDeleting(null)}>
              Cancel
            </Button>
            <Button
              variant="danger-solid"
              disabled={busy()}
              onClick={() => {
                const v = deleting();
                if (v) remove(v);
              }}
            >
              Delete volume
            </Button>
          </>
        }
      >
        <p>The volume and all its data are removed. This can't be undone.</p>
        <Show when={deleting() && mountedBy(deleting()?.name ?? "").length > 0}>
          <Note tone="warn">
            Mounted by{" "}
            {mountedBy(deleting()?.name ?? "")
              .map((d) => d.name)
              .join(", ")}
            .
          </Note>
        </Show>
      </Modal>
    </div>
  );
}
