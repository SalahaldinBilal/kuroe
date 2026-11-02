import { createMemo, createResource, createSignal, For, Show } from "solid-js";
import { api, type DeploymentDetail, type Reach } from "../api";
import useToastState, { errorMessage } from "../states/toastState";
import { Card, Note, Pill } from "./ui/Blocks";
import Button from "./ui/Button/Button";
import EmptyState from "./ui/EmptyState/EmptyState";
import Field from "./ui/Field/Field";
import { Icon } from "./ui/Icon";
import Modal from "./ui/Modal/Modal";
import { ready } from "./ui/Overlay";
import Select from "./ui/Select/Select";
import { Busy, SkeletonTable } from "./ui/Skeleton";

const VIA: Record<Reach["via"], string> = {
  owner: "Owner",
  grant: "Added here",
  role: "Their server role",
};

export function AccessPanel(props: { detail: DeploymentDetail }) {
  const { pushToast, pushError } = useToastState;
  const [reach, { refetch }] = createResource(() => props.detail.id, api.deploymentAccess);
  const [users] = createResource(api.users);
  const [roles] = createResource(api.roles);
  const [adding, setAdding] = createSignal(false);
  const [user, setUser] = createSignal("");
  const [role, setRole] = createSignal("deployer");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const descriptions = createMemo(() => new Map((ready(roles) ?? []).map((r) => [r.name, r.description])));
  const roleItems = () =>
    (ready(roles) ?? []).filter((r) => r.name !== "owner").map((r) => ({ value: r.name, label: r.name }));
  const candidates = () => {
    const granted = new Set((ready(reach) ?? []).filter((r) => r.via !== "role").map((r) => r.user_id));
    return (ready(users) ?? [])
      .filter((u) => u.role !== "owner" && !granted.has(u.id))
      .map((u) => ({ value: u.id, label: u.username }));
  };

  function open() {
    setUser(candidates()[0]?.value ?? "");
    setRole("deployer");
    setError("");
    setAdding(true);
  }

  async function grant(event: SubmitEvent) {
    event.preventDefault();
    setError("");
    setBusy(true);
    try {
      await api.setGrant(props.detail.id, user(), role());
      pushToast(`Gave ${candidates().find((c) => c.value === user())?.label} ${role()} on ${props.detail.name}`);
      setAdding(false);
      refetch();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  async function revoke(r: Reach) {
    try {
      await api.removeGrant(props.detail.id, r.user_id);
      pushToast(`${r.username} is back to their server role here`);
      refetch();
    } catch (err) {
      pushError(err);
    }
  }

  return (
    <Card
      title={`Who can access ${props.detail.name}`}
      description="Everyone whose role covers this deployment, plus people added here"
      actions={
        <Button variant="primary" size="sm" disabled={candidates().length === 0} onClick={open}>
          <Icon name="plus" />
          Add person
        </Button>
      }
    >
      <Show
        when={ready(reach)}
        fallback={
          <Show
            when={reach.error}
            fallback={
              <Busy>
                <SkeletonTable bare rows={1} columns={4} />
              </Busy>
            }
          >
            <EmptyState
              compact
              icon="alert"
              tone="danger"
              title="Couldn't load access"
              description={String(reach.error?.message)}
            />
          </Show>
        }
      >
        {(list) => (
          <table class="tbl">
            <thead>
              <tr>
                <th>User</th>
                <th>Role here</th>
                <th class="hide-sm">Comes from</th>
                <th />
              </tr>
            </thead>
            <tbody>
              <For each={list()}>
                {(r) => (
                  <tr>
                    <td>
                      <span class="hrow" style={{ gap: "10px" }}>
                        <span class={r.via === "owner" ? "av" : "av soft"}>{r.username.slice(0, 2)}</span>
                        <b style={{ "font-weight": "500", color: "var(--ink)" }}>{r.username}</b>
                      </span>
                    </td>
                    <td>
                      <Pill tone={r.via === "grant" ? "ok" : undefined} tooltip={descriptions().get(r.role)}>
                        {r.role}
                      </Pill>
                    </td>
                    <td class="hide-sm muted">{VIA[r.via]}</td>
                    <td class="acts">
                      <Show
                        when={r.via === "grant"}
                        fallback={
                          <Show when={r.via === "role"}>
                            <Button variant="ghost" size="sm" href="/users">
                              Change role
                            </Button>
                          </Show>
                        }
                      >
                        <Button variant="ghost" icon size="sm" tooltip="Remove access" onClick={() => revoke(r)}>
                          <Icon name="x" />
                        </Button>
                      </Show>
                    </td>
                  </tr>
                )}
              </For>
            </tbody>
          </table>
        )}
      </Show>

      <Modal
        show={adding()}
        title={`Give access to ${props.detail.name}`}
        onHide={() => setAdding(false)}
        footer={
          <>
            <Button variant="ghost" onClick={() => setAdding(false)}>
              Cancel
            </Button>
            <Button type="submit" form="grant-access" variant="primary" disabled={busy() || !user()}>
              Give access
            </Button>
          </>
        }
      >
        <form id="grant-access" class="stack" style={{ gap: "14px" }} onSubmit={grant}>
          <div class="grid2">
            <Field label="User">
              <Select label="User" items={candidates()} value={user()} onChange={setUser} />
            </Field>
            <Field label="Role on this deployment">
              <Select label="Role on this deployment" items={roleItems()} value={role()} onChange={setRole} />
            </Field>
          </div>
          <span class="hint">
            Only changes what they can do on {props.detail.name}. Their role elsewhere stays the same.
          </span>
          <Show when={error()}>
            <Note tone="bad">{error()}</Note>
          </Show>
        </form>
      </Modal>
    </Card>
  );
}
