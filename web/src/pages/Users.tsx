import { createMemo, createResource, createSignal, For, Index, type Resource, Show } from "solid-js";
import { useUser } from "../App";
import { api, type Grant, isOwner, type Permission, type RoleDef, type RoleInput, type UserRow } from "../api";
import { ChangePassword } from "../components/ChangePassword";
import { Card, Chevron, Note, PageHeader, Pill } from "../components/ui/Blocks";
import Button from "../components/ui/Button/Button";
import EmptyState from "../components/ui/EmptyState/EmptyState";
import Field from "../components/ui/Field/Field";
import { Tiles } from "../components/ui/Form";
import { Icon } from "../components/ui/Icon";
import Input from "../components/ui/Input/Input";
import Modal from "../components/ui/Modal/Modal";
import { ready } from "../components/ui/Overlay";
import SegmentedControl from "../components/ui/SegmentedControl/SegmentedControl";
import Select from "../components/ui/Select/Select";
import { Busy, SkeletonList, SkeletonTable } from "../components/ui/Skeleton";
import Switch from "../components/ui/Switch/Switch";
import Tabs from "../components/ui/Tabs/Tabs";
import { formatDate } from "../format";
import { ALL_PERMISSIONS, PERMISSION_GROUPS, scopeLabel, togglePermission } from "../permissions";
import useToastState, { errorMessage } from "../states/toastState";

type Tab = "users" | "roles";

export function Users() {
  const me = useUser();
  const { pushToast } = useToastState;
  const [tab, setTab] = createSignal<Tab>("users");
  const [users, { refetch }] = createResource(api.users);
  const [roles, { refetch: refetchRoles }] = createResource(api.roles);
  const [deployments] = createResource(api.deployments);
  const [adding, setAdding] = createSignal(false);
  const [editing, setEditing] = createSignal<UserRow | null>(null);
  const [editingRole, setEditingRole] = createSignal<RoleDraft | null>(null);
  const [changingPassword, setChangingPassword] = createSignal(false);
  const [username, setUsername] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [role, setRole] = createSignal("viewer");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const roleList = () => ready(roles) ?? [];
  const roleByName = createMemo(() => new Map(roleList().map((r) => [r.name, r])));
  const assignable = () => roleList().filter((r) => isOwner(me()) || r.name !== "owner");
  const deploymentNames = createMemo(() => new Map((ready(deployments) ?? []).map((d) => [d.id, d.name])));
  const owners = () => (ready(users) ?? []).filter((u) => isOwner(u)).length;

  function open() {
    setUsername("");
    setPassword("");
    setRole("viewer");
    setError("");
    setAdding(true);
  }

  async function add(event: SubmitEvent) {
    event.preventDefault();
    setError("");
    setBusy(true);
    try {
      await api.createUser(username().trim(), password(), role());
      pushToast(`Added ${username().trim()}`);
      setAdding(false);
      refetch();
      refetchRoles();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  function newRole() {
    setEditingRole({
      role: { name: "", builtin: false, description: "", permissions: ["dep.view"], scope: "all", users: 0 },
      isNew: true,
    });
  }

  return (
    <div class="content narrow">
      <PageHeader
        title="Users"
        description="Who can sign in and what they can do"
        actions={
          <Show
            when={tab() === "users"}
            fallback={
              <Button variant="primary" onClick={newRole}>
                <Icon name="plus" />
                New role
              </Button>
            }
          >
            <Button variant="primary" onClick={open}>
              <Icon name="plus" />
              Add user
            </Button>
          </Show>
        }
      />
      <Tabs
        tabs={[
          { value: "users", label: "Users", count: ready(users)?.length },
          { value: "roles", label: "Roles", count: ready(roles)?.length },
        ]}
        value={tab()}
        onChange={setTab}
      />
      <Show
        when={tab() === "users"}
        fallback={
          <RolesList
            roles={roles}
            names={deploymentNames()}
            onOpen={(role) => setEditingRole({ role, isNew: false })}
          />
        }
      >
        <div class="stack">
          <Card>
            <Show
              when={ready(users)}
              fallback={
                <Show
                  when={users.error}
                  fallback={
                    <Busy>
                      <SkeletonTable bare rows={1} columns={4} />
                    </Busy>
                  }
                >
                  <EmptyState
                    icon="alert"
                    tone="danger"
                    title="Couldn't load users"
                    description={String(users.error?.message)}
                  />
                </Show>
              }
            >
              {(list) => (
                <table class="tbl">
                  <thead>
                    <tr>
                      <th>User</th>
                      <th>Role</th>
                      <th>Host mounts</th>
                      <th class="hide-sm">Created</th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={list()}>
                      {(u) => (
                        <tr class="click" onClick={() => setEditing(u)}>
                          <td>
                            <span class="hrow" style={{ gap: "10px" }}>
                              <span class={isOwner(u) ? "av" : "av soft"}>{u.username.slice(0, 2)}</span>
                              <b style={{ "font-weight": "500", color: "var(--ink)" }}>{u.username}</b>
                              <Show when={u.id === me().id}>
                                <span class="faint">you</span>
                              </Show>
                            </span>
                          </td>
                          <td>
                            <span class="hrow" style={{ gap: "6px" }}>
                              <Pill
                                tone={isOwner(u) ? "bad" : undefined}
                                tooltip={roleByName().get(u.role)?.description}
                              >
                                {u.role}
                              </Pill>
                              <Show when={u.grants > 0}>
                                <span class="faint">
                                  +{u.grants} {u.grants === 1 ? "deployment" : "deployments"}
                                </span>
                              </Show>
                            </span>
                          </td>
                          <td>
                            <Show when={isOwner(u) || u.host_mounts} fallback={<span class="faint">off</span>}>
                              <Pill tone="warn">allowed</Pill>
                            </Show>
                          </td>
                          <td class="hide-sm mono muted">{formatDate(u.created_at)}</td>
                        </tr>
                      )}
                    </For>
                  </tbody>
                </table>
              )}
            </Show>
          </Card>
          <Note tone="warn">
            Host mounts let a user bind any host path into a container. Only turn it on for people you'd give root on
            this machine.
          </Note>
        </div>
      </Show>

      <Modal
        show={adding()}
        title="Add user"
        onHide={() => setAdding(false)}
        footer={
          <>
            <Button variant="ghost" onClick={() => setAdding(false)}>
              Cancel
            </Button>
            <Button type="submit" form="add-user" variant="primary" disabled={busy()}>
              Add user
            </Button>
          </>
        }
      >
        <form id="add-user" class="stack" style={{ gap: "14px" }} onSubmit={add}>
          <Field label="Username">
            <Input autofocus required label="Username" value={username()} onValue={setUsername} />
          </Field>
          <Field label="Password" hint="At least 10 characters.">
            <Input
              type="password"
              required
              minLength={10}
              label="Password"
              autocomplete="new-password"
              value={password()}
              onValue={setPassword}
            />
          </Field>
          <Field label="Role" hint={roleByName().get(role())?.description}>
            <Select
              label="Role"
              items={assignable().map((r) => ({ value: r.name, label: r.name }))}
              value={role()}
              onChange={setRole}
            />
          </Field>
          <Show when={error()}>
            <Note tone="bad">{error()}</Note>
          </Show>
        </form>
      </Modal>

      <Show when={editing()}>
        {(u) => (
          <EditUser
            user={u()}
            isMe={u().id === me().id}
            lastOwner={isOwner(u()) && owners() <= 1}
            roles={assignable()}
            deployments={deploymentNames()}
            onClose={() => setEditing(null)}
            onChangeOwnPassword={() => {
              setEditing(null);
              setChangingPassword(true);
            }}
            onSaved={() => {
              setEditing(null);
              refetch();
              refetchRoles();
            }}
          />
        )}
      </Show>
      <Show when={editingRole()} keyed>
        {(draft) => (
          <RoleEditor
            draft={draft}
            deployments={deploymentNames()}
            onClose={() => setEditingRole(null)}
            onDuplicate={() => {
              const r = draft.role;
              setEditingRole({
                role: { ...r, name: `${r.name}-copy`, builtin: false, users: 0, scope: copyScope(r) },
                isNew: true,
              });
            }}
            onSaved={() => {
              setEditingRole(null);
              refetchRoles();
              refetch();
            }}
          />
        )}
      </Show>
      <ChangePassword show={changingPassword()} onHide={() => setChangingPassword(false)} />
    </div>
  );
}

function copyScope(role: RoleDef): RoleDef["scope"] {
  return role.scope === "granted" ? "all" : role.scope;
}

function RolesList(props: { roles: Resource<RoleDef[]>; names: Map<string, string>; onOpen: (role: RoleDef) => void }) {
  return (
    <div class="stack">
      <Card title="Roles" description="Built-in roles can't be edited. Duplicate one to start a custom role.">
        <Show
          when={ready(props.roles)}
          fallback={
            <Show
              when={props.roles.error}
              fallback={
                <Busy>
                  <SkeletonList bare rows={4} icon={false} actions={1} />
                </Busy>
              }
            >
              <EmptyState
                icon="alert"
                tone="danger"
                title="Couldn't load roles"
                description={String(props.roles.error?.message)}
              />
            </Show>
          }
        >
          {(list) => (
            <For each={list()}>
              {(r) => (
                <button type="button" class="list-i" onClick={() => props.onOpen(r)}>
                  <Icon name={r.builtin ? "lock" : "shield"} />
                  <div class="nm grow">
                    <b>
                      {r.name} <Pill>{r.builtin ? "Built in" : "Custom"}</Pill>
                    </b>
                    <span class="plain">{r.description || scopeLabel(r.scope, props.names)}</span>
                  </div>
                  <span class="muted hide-sm">{scopeLabel(r.scope, props.names)}</span>
                  <span class="mono muted hide-sm">
                    {r.permissions.length}/{ALL_PERMISSIONS.length}
                  </span>
                  <span class="muted">
                    {r.users} {r.users === 1 ? "user" : "users"}
                  </span>
                  <Chevron />
                </button>
              )}
            </For>
          )}
        </Show>
      </Card>
      <Note>
        A user gets their role everywhere it applies. To give one person a different role on a single deployment, edit
        the user or use the deployment's Access tab.
      </Note>
    </div>
  );
}

const SCOPE_OPTIONS: { value: "all" | "some"; label: string }[] = [
  { value: "all", label: "All deployments" },
  { value: "some", label: "Only selected" },
];

interface RoleDraft {
  role: RoleDef;
  isNew: boolean;
}

function RoleEditor(props: {
  draft: RoleDraft;
  deployments: Map<string, string>;
  onClose: () => void;
  onDuplicate: () => void;
  onSaved: () => void;
}) {
  const { pushToast } = useToastState;
  const role = props.draft.role;
  const readOnly = () => role.builtin;
  const exists = () => !props.draft.isNew;
  const [name, setName] = createSignal(role.name);
  const [description, setDescription] = createSignal(role.description);
  const [permissions, setPermissions] = createSignal<Permission[]>(role.permissions);
  const [scope, setScope] = createSignal(role.scope);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [deleting, setDeleting] = createSignal(false);
  const original = role.name;

  const selected = () => {
    const s = scope();
    return Array.isArray(s) ? s : [];
  };

  function toggleDeployment(id: string, on: boolean) {
    const rest = selected().filter((d) => d !== id);
    setScope(on ? [...rest, id] : rest);
  }

  async function run(action: () => Promise<void>) {
    setError("");
    setBusy(true);
    try {
      await action();
      props.onSaved();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  function save(event: SubmitEvent) {
    event.preventDefault();
    const input: RoleInput = {
      name: name().trim(),
      description: description().trim(),
      permissions: permissions(),
      scope: scope(),
    };
    run(async () => {
      if (exists()) await api.updateRole(original, input);
      else await api.createRole(input);
      pushToast(`Saved role ${input.name}`);
    });
  }

  function remove() {
    run(async () => {
      await api.removeRole(original);
      pushToast(`Deleted role ${original}`);
    });
  }

  const title = () => (readOnly() ? role.name : exists() ? `Edit ${role.name}` : "New role");

  return (
    <Modal
      show
      width={640}
      title={title()}
      onHide={props.onClose}
      footer={
        <Show
          when={!readOnly()}
          fallback={
            <>
              <Button variant="ghost" onClick={props.onClose}>
                Close
              </Button>
              <Button variant="primary" onClick={props.onDuplicate}>
                <Icon name="copy" />
                Duplicate
              </Button>
            </>
          }
        >
          <Show when={exists() && !deleting()}>
            <Button variant="danger" onClick={() => setDeleting(true)}>
              Delete role
            </Button>
            <span class="grow" />
          </Show>
          <Show
            when={!deleting()}
            fallback={
              <>
                <span class="grow">Delete {original}? Users on it need another role first.</span>
                <Button variant="ghost" onClick={() => setDeleting(false)}>
                  Back
                </Button>
                <Button variant="danger-solid" disabled={busy()} onClick={remove}>
                  Delete role
                </Button>
              </>
            }
          >
            <Button variant="ghost" onClick={props.onClose}>
              Cancel
            </Button>
            <Button type="submit" form="edit-role" variant="primary" disabled={busy() || !name().trim()}>
              Save role
            </Button>
          </Show>
        </Show>
      }
    >
      <form id="edit-role" class="stack" style={{ gap: "14px" }} onSubmit={save}>
        <Show when={readOnly()}>
          <Note icon="lock">Built-in roles can't be edited. Duplicate it to make your own.</Note>
        </Show>
        <div class="grid2">
          <Field label="Name">
            <Input
              mono
              autofocus={props.draft.isNew}
              readOnly={readOnly()}
              label="Role name"
              placeholder="frontend"
              value={name()}
              onValue={setName}
            />
          </Field>
          <Field label="Description">
            <Input
              readOnly={readOnly()}
              label="Description"
              placeholder="What this role is for"
              value={description()}
              onValue={setDescription}
            />
          </Field>
        </div>
        <Field
          label="Applies to"
          hint="Deployment permissions only work on these. New deployments are included when this is All deployments."
        >
          <Show
            when={scope() !== "granted"}
            fallback={<Note>Deployments they're granted. On every other deployment they can only view.</Note>}
          >
            <SegmentedControl
              label="Applies to"
              value={scope() === "all" ? "all" : "some"}
              options={SCOPE_OPTIONS}
              onChange={(v) => !readOnly() && setScope(v === "all" ? "all" : selected())}
            />
            <Show when={scope() !== "all"}>
              <div class="perm-grid boxed">
                <For each={[...props.deployments]} fallback={<span class="hint">No deployments yet.</span>}>
                  {([id, label]) => (
                    <Switch
                      label={<span class="mono">{label}</span>}
                      checked={selected().includes(id)}
                      disabled={readOnly()}
                      onChange={(on) => toggleDeployment(id, on)}
                    />
                  )}
                </For>
              </div>
            </Show>
          </Show>
        </Field>
        <For each={PERMISSION_GROUPS}>
          {([group, list]) => (
            <Field label={group}>
              <div class="perm-grid">
                <For each={list}>
                  {([id, label]) => (
                    <Switch
                      label={label}
                      checked={permissions().includes(id)}
                      disabled={readOnly()}
                      onChange={(on) => setPermissions(togglePermission(permissions(), id, on))}
                    />
                  )}
                </For>
              </div>
            </Field>
          )}
        </For>
        <Show when={error()}>
          <Note tone="bad">{error()}</Note>
        </Show>
      </form>
    </Modal>
  );
}

interface EditUserProps {
  user: UserRow;
  isMe: boolean;
  lastOwner: boolean;
  roles: RoleDef[];
  deployments: Map<string, string>;
  onClose: () => void;
  onSaved: () => void;
  onChangeOwnPassword: () => void;
}

function sameGrants(a: Grant[], b: Grant[]): boolean {
  return a.length === b.length && a.every((g, i) => g.deployment_id === b[i].deployment_id && g.role === b[i].role);
}

function EditUser(props: EditUserProps) {
  const { pushToast } = useToastState;
  const owner = () => isOwner(props.user);
  const [role, setRole] = createSignal(props.user.role);
  const [mounts, setMounts] = createSignal(props.user.host_mounts);
  const [password, setPassword] = createSignal("");
  const [savedGrants] = createResource(() => (owner() ? null : props.user.id), api.userGrants);
  const [grants, setGrants] = createSignal<Grant[] | null>(null);
  const [removing, setRemoving] = createSignal(false);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const currentGrants = () => grants() ?? ready(savedGrants) ?? [];
  const roleTiles = () => props.roles.map((r) => ({ value: r.name, label: r.name, description: r.description }));
  const roleItems = () => props.roles.filter((r) => r.name !== "owner").map((r) => ({ value: r.name, label: r.name }));
  const deploymentItems = (keep: string) =>
    [...props.deployments]
      .filter(([id]) => id === keep || !currentGrants().some((g) => g.deployment_id === id))
      .map(([id, name]) => ({ value: id, label: name }));

  function patchGrant(i: number, change: Partial<Grant>) {
    setGrants(currentGrants().map((g, j) => (j === i ? { ...g, ...change } : g)));
  }

  function addGrant() {
    const free = deploymentItems("")[0];
    if (!free) return;
    setGrants([...currentGrants(), { deployment_id: free.value, role: "deployer" }]);
  }

  async function run(action: () => Promise<void>) {
    setError("");
    setBusy(true);
    try {
      await action();
      props.onSaved();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  function save(event: SubmitEvent) {
    event.preventDefault();
    run(async () => {
      const u = props.user;
      if (password() && password().length < 10) throw new Error("The new password needs at least 10 characters");
      if (role() !== u.role || mounts() !== u.host_mounts) await api.updateUser(u.id, role(), mounts());
      const next = grants();
      if (next && !sameGrants(next, ready(savedGrants) ?? [])) await api.setUserGrants(u.id, next);
      if (password()) await api.setUserPassword(u.id, password());
      pushToast(password() ? `Saved ${u.username}. They were signed out everywhere.` : `Saved ${u.username}`);
    });
  }

  function remove() {
    run(async () => {
      await api.removeUser(props.user.id);
      pushToast(`Removed ${props.user.username}`);
    });
  }

  const title = (
    <span class="hrow" style={{ gap: "10px" }}>
      <span class={owner() ? "av" : "av soft"}>{props.user.username.slice(0, 2)}</span>
      {props.user.username}
      <span class="faint" style={{ "font-weight": "400", "font-size": "12.5px" }}>
        since {formatDate(props.user.created_at)}
      </span>
    </span>
  );

  return (
    <>
      <Modal
        show={!removing()}
        title={title}
        onHide={props.onClose}
        footer={
          <>
            <Button variant="ghost" onClick={props.onClose}>
              Cancel
            </Button>
            <Button type="submit" form="edit-user" variant="primary" disabled={busy()}>
              Save
            </Button>
          </>
        }
      >
        <form id="edit-user" class="stack" style={{ gap: "14px" }} onSubmit={save}>
          <Field label="Role">
            <Show when={!owner()} fallback={<Note>An owner's role can't be changed here.</Note>}>
              <Tiles columns={1} label="Role" options={roleTiles()} value={role()} onChange={setRole} />
            </Show>
          </Field>
          <Show when={!owner()}>
            <Field
              label="Different role on single deployments"
              hint="Overrides their role on that deployment only. Handy for a viewer who deploys one app."
            >
              <div class="kv">
                <Index each={currentGrants()}>
                  {(g, i) => (
                    <div class="kvrow">
                      <Select
                        mono
                        class="k"
                        label="Deployment"
                        items={deploymentItems(g().deployment_id)}
                        value={g().deployment_id}
                        onChange={(deployment_id) => patchGrant(i, { deployment_id })}
                      />
                      <Select
                        label="Role on this deployment"
                        items={roleItems()}
                        value={g().role}
                        onChange={(r) => patchGrant(i, { role: r })}
                      />
                      <Button
                        variant="ghost"
                        icon
                        size="sm"
                        tooltip="Remove"
                        onClick={() => setGrants(currentGrants().filter((_, j) => j !== i))}
                      >
                        <Icon name="x" />
                      </Button>
                    </div>
                  )}
                </Index>
                <div class="kv-actions">
                  <Button size="sm" disabled={deploymentItems("").length === 0} onClick={addGrant}>
                    <Icon name="plus" />
                    Add deployment
                  </Button>
                </div>
              </div>
            </Field>
          </Show>
          <Field
            label="Host mounts"
            hint="This is root access to the host. Only for people you'd give root on this machine."
          >
            <Switch
              label="Allow bind mounts of host paths"
              checked={owner() || mounts()}
              disabled={owner()}
              onChange={setMounts}
            />
          </Field>
          <Show
            when={!props.isMe}
            fallback={
              <Field label="Password">
                <div>
                  <Button onClick={props.onChangeOwnPassword}>
                    <Icon name="key" />
                    Change your password
                  </Button>
                </div>
              </Field>
            }
          >
            <Field
              label="Set a new password"
              hint="At least 10 characters. They are signed out everywhere and use the new one next time."
            >
              <Input
                type="password"
                label="New password"
                autocomplete="new-password"
                placeholder="leave empty to keep the current one"
                value={password()}
                onValue={setPassword}
              />
            </Field>
          </Show>
          <Show when={!props.isMe && !props.lastOwner}>
            <Note
              tone="bad"
              icon="trash"
              action={
                <Button size="sm" variant="danger" onClick={() => setRemoving(true)}>
                  Remove user
                </Button>
              }
            >
              Remove {props.user.username} from this server.
            </Note>
          </Show>
          <Show when={error()}>
            <Note tone="bad">{error()}</Note>
          </Show>
        </form>
      </Modal>

      <Modal
        show={removing()}
        title={`Remove ${props.user.username}?`}
        onHide={props.onClose}
        footer={
          <>
            <Button variant="ghost" onClick={() => setRemoving(false)}>
              Back
            </Button>
            <Button variant="danger-solid" disabled={busy()} onClick={remove}>
              Remove user
            </Button>
          </>
        }
      >
        <p>They can't sign in anymore and their sessions end now. Deployments they created stay.</p>
        <Show when={error()}>
          <Note tone="bad">{error()}</Note>
        </Show>
      </Modal>
    </>
  );
}
