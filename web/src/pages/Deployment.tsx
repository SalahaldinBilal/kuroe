import { A, useNavigate, useParams, useSearchParams } from "@solidjs/router";
import { createEffect, createResource, createSignal, For, Match, onCleanup, Show, Switch } from "solid-js";
import { useUser } from "../App";
import { api, type ContainerRow, can, type DeploymentDetail, type Permission, socketUrl } from "../api";
import { AccessPanel } from "../components/AccessPanel";
import { AnsiText } from "../components/AnsiText";
import { CiPanel } from "../components/CiPanel";
import { DeployHistory } from "../components/DeployHistory";
import { DeploymentMetrics } from "../components/DeploymentMetrics";
import { LiveDeploy } from "../components/DeployRun";
import { Domains } from "../components/Domains";
import { type FileCache, Files } from "../components/Files";
import { LogHistory } from "../components/LogHistory";
import { SiteSettings } from "../components/SiteSettings";
import { Terminal } from "../components/Terminal";
import { Card, Note, StatusBadge } from "../components/ui/Blocks";
import Button from "../components/ui/Button/Button";
import Drawer from "../components/ui/Drawer/Drawer";
import EmptyState from "../components/ui/EmptyState/EmptyState";
import Field from "../components/ui/Field/Field";
import { Icon } from "../components/ui/Icon";
import Input from "../components/ui/Input/Input";
import Modal from "../components/ui/Modal/Modal";
import { ready } from "../components/ui/Overlay";
import SegmentedControl from "../components/ui/SegmentedControl/SegmentedControl";
import { Bone, Busy, SkeletonCardHead } from "../components/ui/Skeleton";
import Tabs from "../components/ui/Tabs/Tabs";
import Tip from "../components/ui/Tooltip/Tip";
import { LimitCell, SizeCell, volumeLabel } from "../components/VolumeLimit";
import { formatAgo, formatBytes, formatTime } from "../format";
import { isStatic, sourceLabel } from "../spec";
import useContextMenuState, { type MenuItem } from "../states/contextMenuState";
import { eventsConnected, onEvent } from "../states/eventState";
import useToastState from "../states/toastState";
import { KIND_ICON } from "./Deployments";
import { ServiceSettingsForm } from "./ServiceSettings";

type PanelKind = "logs" | "shell" | "attach" | "files";
type Tab = "overview" | "deploys" | "metrics" | "logs" | "domains" | "ci" | "access" | "settings";

interface Panel {
  container: ContainerRow;
  kind: PanelKind;
  exec?: string;
}

const PANEL_OPTIONS: { value: PanelKind; label: string }[] = [
  { value: "logs", label: "Logs" },
  { value: "shell", label: "Shell" },
  { value: "attach", label: "Attach" },
  { value: "files", label: "Files" },
];

const has = (d: DeploymentDetail, permission: Permission) => d.permissions.includes(permission);

export function Deployment() {
  const params = useParams<{ id: string }>();
  const [search, setSearch] = useSearchParams<{ tab?: string; deploy?: string }>();
  const navigate = useNavigate();
  const user = useUser();
  const canManageAccess = () => can(user(), "users");
  const { pushToast, pushError } = useToastState;
  const { openMenuBelow } = useContextMenuState;
  const [detail, { refetch, mutate }] = createResource(() => params.id, api.deployment);
  const [panel, setPanel] = createSignal<Panel | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [deleting, setDeleting] = createSignal(false);
  const [confirmName, setConfirmName] = createSignal("");

  const tab = (): Tab => (search.tab as Tab | undefined) ?? "overview";

  onEvent(
    (event) =>
      (event.kind === "deployment.status" && event.id === params.id) ||
      (event.kind === "container.state" && event.deployment === params.id),
    refetch,
  );

  createEffect(() => {
    if (eventsConnected() || ready(detail)?.status !== "deploying") return;
    const timer = setInterval(refetch, 1500);
    onCleanup(() => clearInterval(timer));
  });

  async function act(action: () => Promise<unknown>, done: string) {
    setBusy(true);
    try {
      await action();
      pushToast(done);
      await refetch();
    } catch (err) {
      pushError(err);
    } finally {
      setBusy(false);
    }
  }

  async function remove(d: DeploymentDetail) {
    setBusy(true);
    try {
      await api.removeDeployment(d.id);
      pushToast(`${d.name} deleted`);
      navigate("/deployments");
    } catch (err) {
      pushError(err);
      setBusy(false);
    }
  }

  async function openPanel(container: ContainerRow, kind: PanelKind) {
    if (kind !== "shell") return setPanel({ container, kind });
    try {
      const exec = await api.createExec(container.id);
      setPanel({ container, kind, exec: exec.id });
    } catch (err) {
      pushError(err);
    }
  }

  function moreMenu(d: DeploymentDetail): MenuItem[] {
    const items: MenuItem[] = [];
    if (has(d, "dep.edit")) {
      items.push({
        label: "Export as TOML",
        icon: "download",
        onSelect: () => window.location.assign(api.exportUrl(d.id, false)),
      });
    }
    if (has(d, "dep.delete")) {
      if (items.length) items.push("separator");
      items.push({
        label: "Delete deployment",
        icon: "trash",
        danger: true,
        onSelect: () => {
          setConfirmName("");
          setDeleting(true);
        },
      });
    }
    return items;
  }

  const tabs = (d: DeploymentDetail) =>
    [
      { value: "overview", label: "Overview" },
      { value: "deploys", label: "Deploys" },
      { value: "metrics", label: "Metrics" },
      { value: "logs", label: "Logs" },
      { value: "domains", label: "Domains" },
      ...(has(d, "dep.ci") ? [{ value: "ci", label: "CI and export" }] : []),
      ...(canManageAccess() ? [{ value: "access", label: "Access" }] : []),
      ...(has(d, "dep.edit") ? [{ value: "settings", label: "Settings" }] : []),
    ] as { value: Tab; label: string }[];

  return (
    <Show
      when={ready(detail)}
      fallback={
        <Show when={detail.error} fallback={<DeploymentSkeleton tab={tab()} />}>
          <div class="content">
            <Card>
              <EmptyState
                icon="alert"
                tone="danger"
                title="Couldn't load this deployment"
                description={String(detail.error?.message)}
                actions={
                  <Button size="sm" href="/deployments">
                    Back to deployments
                  </Button>
                }
              />
            </Card>
          </div>
        </Show>
      }
    >
      {(d) => (
        <div class="content">
          <div class="crumbs">
            <A href="/deployments">Deployments</A>
            <Icon name="right" />
            <span>{d().name}</span>
          </div>
          <div class="dhead">
            <h1>{d().name}</h1>
            <StatusBadge status={d().status} />
            <span class="grow" />
            <Show when={has(d(), "dep.deploy")}>
              <Button
                variant="primary"
                disabled={busy() || d().status === "deploying"}
                onClick={() => act(() => api.deploy(d().id), `Deploying ${d().name}`)}
              >
                <Icon name="play" />
                Deploy
              </Button>
              <Button
                disabled={busy() || (d().status !== "running" && d().status !== "degraded")}
                onClick={() => act(() => api.restart(d().id), `Restarted ${d().name}`)}
              >
                <Icon name="rotate" />
                Restart
              </Button>
              <Button
                disabled={busy() || d().status === "deploying" || d().status === "stopped"}
                onClick={() => act(() => api.stop(d().id), `Stopped ${d().name}`)}
              >
                <Icon name="stop" />
                Stop
              </Button>
            </Show>
            <Show when={moreMenu(d()).length > 0}>
              <Button icon tooltip="More actions" onClick={(e) => openMenuBelow(e.currentTarget, moreMenu(d()))}>
                <Icon name="more" />
              </Button>
            </Show>
          </div>
          <div class="dmeta">
            <span>
              <Icon name={KIND_ICON[d().spec.source.kind]} />
              <span class="mono">{sourceLabel(d().spec)}</span>
            </span>
            <span>
              <Icon name="rotate" />
              Revision {d().revision}
            </span>
            <Tip content={`${formatTime(d().updated_at)} UTC`}>
              <span class="hrow" style={{ gap: "6px" }}>
                <Icon name="clock" />
                Updated {formatAgo(d().updated_at)}
              </span>
            </Tip>
            <Show when={has(d(), "dep.deploy")}>
              <Tip
                content={
                  d().strategy === "blue_green"
                    ? "Deploys start the new version next to the old one and switch traffic once it is healthy."
                    : d().strategy_note
                }
              >
                <span class="hrow" style={{ gap: "6px" }}>
                  <Icon name="shield" />
                  {d().strategy === "blue_green" || d().strategy === "static"
                    ? "Zero-downtime deploys"
                    : "Restarts during deploys"}
                </span>
              </Tip>
            </Show>
          </div>

          <Tabs
            tabs={tabs(d())}
            value={tab()}
            onChange={(value) =>
              setSearch({ tab: value === "overview" ? undefined : value, deploy: undefined }, { replace: true })
            }
          />

          <Switch>
            <Match when={tab() === "overview"}>
              <Overview
                detail={d()}
                selected={panel()?.container.id}
                busy={busy()}
                onOpen={openPanel}
                onDeploy={() => act(() => api.deploy(d().id), `Deploying ${d().name}`)}
                onRefresh={refetch}
                onOpenDeploy={(deploy) => setSearch({ tab: "deploys", deploy })}
              />
            </Match>
            <Match when={tab() === "deploys"}>
              <DeployHistory
                detail={d()}
                open={search.deploy}
                onOpen={(deploy) => setSearch({ deploy }, { replace: true })}
              />
            </Match>
            <Match when={tab() === "metrics"}>
              <DeploymentMetrics
                deployment={d().id}
                services={d().spec.services}
                stack={d().spec.mode === "stack"}
                containers={d().containers}
              />
            </Match>
            <Match when={tab() === "logs"}>
              <LogHistory deployment={d().id} />
            </Match>
            <Match when={tab() === "domains"}>
              <Domains detail={d()} />
            </Match>
            <Match when={tab() === "ci" && has(d(), "dep.ci")}>
              <CiPanel detail={d()} />
            </Match>
            <Match when={tab() === "access" && canManageAccess()}>
              <AccessPanel detail={d()} />
            </Match>
            <Match when={tab() === "settings" && has(d(), "dep.edit")}>
              <Show
                when={isStatic(d().spec)}
                fallback={<ServiceSettingsForm detail={d()} onSaved={(next) => mutate({ ...d(), spec: next })} />}
              >
                <SiteSettings detail={d()} onSaved={(next) => mutate({ ...d(), spec: next })} />
              </Show>
            </Match>
          </Switch>

          <ContainerDrawer
            panel={panel()}
            canShell={has(d(), "dep.shell")}
            onKind={(kind) => {
              const p = panel();
              if (p) openPanel(p.container, kind);
            }}
            onClose={() => setPanel(null)}
          />

          <Modal
            show={deleting()}
            title={`Delete ${d().name}?`}
            onHide={() => setDeleting(false)}
            footer={
              <>
                <Button variant="ghost" onClick={() => setDeleting(false)}>
                  Cancel
                </Button>
                <Button
                  variant="danger-solid"
                  disabled={busy() || confirmName() !== d().name}
                  onClick={() => remove(d())}
                >
                  Delete deployment
                </Button>
              </>
            }
          >
            <p>Its containers are removed. Shared volumes are kept.</p>
            <Field
              label={
                <>
                  Type <span class="mono">{d().name}</span> to confirm
                </>
              }
            >
              <Input mono autofocus label="Deployment name" value={confirmName()} onValue={setConfirmName} />
            </Field>
          </Modal>
        </div>
      )}
    </Show>
  );
}

const SKELETON_TABS: { value: Tab; label: string; disabled: boolean }[] = [
  { value: "overview", label: "Overview", disabled: true },
  { value: "deploys", label: "Deploys", disabled: true },
  { value: "metrics", label: "Metrics", disabled: true },
  { value: "logs", label: "Logs", disabled: true },
  { value: "domains", label: "Domains", disabled: true },
];

/** The deployment page's header, tabs and overview, while the deployment loads. */
function DeploymentSkeleton(props: { tab: Tab }) {
  return (
    <div class="content">
      <Busy>
        <div class="crumbs">
          <Bone w="180px" size="sm" />
        </div>
        <div class="dhead">
          <span class="bone title-bone" />
          <Bone w="72px" size="sm-control" />
          <span class="grow" />
          <Bone w="96px" size="control" />
          <Bone w="96px" size="control" />
          <Bone w="80px" size="control" />
        </div>
        <div class="dmeta">
          <Bone w="260px" />
          <Bone w="90px" />
          <Bone w="120px" />
        </div>
        <Tabs tabs={SKELETON_TABS} value={props.tab} onChange={() => {}} />
        <div class="stack">
          <section class="card">
            <SkeletonCardHead />
            <div class="crow">
              <span class="dot" />
              <div class="cmain">
                <Bone w="240px" />
                <Bone w="160px" size="sm" />
              </div>
              <Bone w="72px" size="sm-control" />
              <Bone w="72px" size="sm-control" />
              <Bone w="72px" size="sm-control" />
            </div>
          </section>
        </div>
      </Busy>
    </div>
  );
}

interface OverviewProps {
  detail: DeploymentDetail;
  selected?: string;
  busy: boolean;
  onOpen: (container: ContainerRow, kind: PanelKind) => void;
  onDeploy: () => void;
  onRefresh: () => void;
  onOpenDeploy: (deploy: string) => void;
}

function Overview(props: OverviewProps) {
  const d = () => props.detail;
  const stopped = () => d().containers.filter((c) => c.state !== "running");
  const [volumes] = createResource(() => d().id, api.volumesOf);
  const over = () => (ready(volumes) ?? []).filter((v) => v.over);
  const { pushError } = useToastState;
  const [confirmCancel, setConfirmCancel] = createSignal(false);
  const [cancelling, setCancelling] = createSignal<number | null>(null);
  const isCancelling = () => cancelling() === d().revision && d().status === "deploying";

  async function cancelDeploy() {
    setConfirmCancel(false);
    setCancelling(d().revision);
    try {
      await api.cancelDeploy(d().id);
    } catch (err) {
      setCancelling(null);
      pushError(err);
    }
  }
  let output: HTMLDetailsElement | undefined;

  function showOutput() {
    if (!output) return;
    output.open = true;
    const term = output.querySelector("pre");
    if (term) term.scrollTop = term.scrollHeight;
    output.scrollIntoView({ behavior: "smooth", block: "nearest" });
    output.classList.remove("flash");
    // Reading layout restarts the highlight on a repeated click.
    void output.offsetWidth;
    output.classList.add("flash");
  }

  return (
    <div class="stack">
      <Switch>
        <Match when={d().status === "deploying"}>
          <Card>
            <div class="card-b stack" style={{ gap: "12px" }}>
              <div class="hrow">
                <b style={{ color: "var(--ink)", "font-weight": "500" }}>
                  {isCancelling() ? "Cancelling" : "Deploying"}
                </b>
                <span class="muted grow">{sourceLabel(d().spec)}</span>
                <Show when={has(d(), "dep.deploy")}>
                  <Button size="sm" disabled={isCancelling()} onClick={() => setConfirmCancel(true)}>
                    <Icon name="x" />
                    {isCancelling() ? "Cancelling" : "Cancel deploy"}
                  </Button>
                </Show>
              </div>
              <LiveDeploy deployment={d().id} onEnd={props.onRefresh} />
            </div>
          </Card>
        </Match>
        <Match when={d().status === "failed"}>
          <Note
            tone="bad"
            action={
              <Show when={d().last_output}>
                <Button size="sm" onClick={showOutput}>
                  See output
                </Button>
              </Show>
            }
          >
            <b>The last deploy failed.</b>
            <Show when={d().last_output}> The output below says why.</Show>
          </Note>
        </Match>
        <Match when={d().status === "degraded" && stopped().length > 0}>
          <Note tone="warn">
            <b>
              {stopped().length} of {d().containers.length} containers stopped:
            </b>{" "}
            <span class="mono">
              {stopped()
                .map((c) => c.name)
                .join(", ")}
            </span>
          </Note>
        </Match>
      </Switch>

      <Modal
        show={confirmCancel()}
        title="Cancel this deploy?"
        onHide={() => setConfirmCancel(false)}
        footer={
          <>
            <Button variant="ghost" onClick={() => setConfirmCancel(false)}>
              Keep deploying
            </Button>
            <Button variant="danger-solid" onClick={cancelDeploy}>
              Cancel deploy
            </Button>
          </>
        }
      >
        <p>
          kuroe stops the build or start where it is and takes down the half-started version. With blue/green, the
          version from before keeps serving. A deployment that restarts in place may already be down.
        </p>
      </Modal>

      <For each={over()}>
        {(v) => (
          <Note tone={v.limit?.stop ? "bad" : "warn"}>
            <b>
              {volumeLabel(v)} is over its limit, {formatBytes(v.bytes ?? 0)} of {formatBytes(v.limit?.max_bytes ?? 0)}.
            </b>{" "}
            {v.limit?.stop
              ? "kuroe stops this deployment while it's over. Free up space or raise the limit on the Volumes page."
              : "Free up space or raise the limit on the Volumes page."}
          </Note>
        )}
      </For>

      <Card
        title="Containers"
        actions={
          <>
            <Show when={has(d(), "dep.shell") && d().containers.some((c) => c.state === "running")}>
              <Button size="sm" href={`/deployments/${d().id}/preview`}>
                <Icon name="activity" />
                Preview
              </Button>
            </Show>
            <span class="count">{d().containers.length}</span>
          </>
        }
      >
        <Show
          when={d().containers.length > 0}
          fallback={
            <EmptyState
              icon="box"
              title="No containers"
              description={d().status === "deploying" ? "The deploy starts them." : "Deploy to start them."}
              actions={
                <Show when={has(d(), "dep.deploy") && d().status !== "deploying"}>
                  <Button variant="primary" size="sm" disabled={props.busy} onClick={props.onDeploy}>
                    <Icon name="play" />
                    Deploy
                  </Button>
                </Show>
              }
            />
          }
        >
          <For each={d().containers}>
            {(c) => {
              const running = () => c.state === "running";
              return (
                <div class={props.selected === c.id ? "crow sel" : "crow"}>
                  <span class={running() ? "dot ok" : "dot bad"} />
                  <div class="cmain">
                    <b>{c.name}</b>
                    <span>
                      {c.service ?? "app"} · {c.status ?? c.state}
                    </span>
                  </div>
                  <div class="cports">
                    <For each={c.ports}>{(p) => <span class="chip">{p.replace("->", " → ")}</span>}</For>
                    <Show when={has(d(), "dep.shell") && running()}>
                      <For each={c.private_ports}>
                        {(port) => (
                          <Tip content="Send requests to this port">
                            <A class="chip" href={`/deployments/${d().id}/preview?container=${c.id}&port=${port}`}>
                              :{port}
                              <Icon name="right" />
                            </A>
                          </Tip>
                        )}
                      </For>
                    </Show>
                  </div>
                  <div class="cact">
                    <Button variant="ghost" size="sm" onClick={() => props.onOpen(c, "logs")}>
                      <Icon name="logs" />
                      Logs
                    </Button>
                    <Show when={has(d(), "dep.shell") && running()}>
                      <Button variant="ghost" size="sm" onClick={() => props.onOpen(c, "shell")}>
                        <Icon name="terminal" />
                        Shell
                      </Button>
                      <Button variant="ghost" size="sm" onClick={() => props.onOpen(c, "attach")}>
                        <Icon name="plug" />
                        Attach
                      </Button>
                      <Button variant="ghost" size="sm" onClick={() => props.onOpen(c, "files")}>
                        <Icon name="folder" />
                        Files
                      </Button>
                    </Show>
                  </div>
                </div>
              );
            }}
          </For>
        </Show>
      </Card>

      <Show when={(ready(volumes) ?? []).length > 0}>
        <Card title="Volumes" description="Measured every 10 minutes">
          <table class="tbl">
            <thead>
              <tr>
                <th>Volume</th>
                <th>Size</th>
                <th>Limit</th>
              </tr>
            </thead>
            <tbody>
              <For each={ready(volumes)}>
                {(v) => (
                  <tr>
                    <td>
                      <span class="hrow">
                        <Icon name={v.shared ? "drive" : "db"} />
                        <span class="mono">{volumeLabel(v)}</span>
                        <Show when={v.shared}>
                          <span class="faint">shared</span>
                        </Show>
                      </span>
                    </td>
                    <td>
                      <SizeCell volume={v} />
                    </td>
                    <td>
                      <LimitCell volume={v} />
                    </td>
                  </tr>
                )}
              </For>
            </tbody>
          </table>
        </Card>
      </Show>

      <Show when={d().last_output && d().status !== "deploying"}>
        <details class="card out" ref={output} open={d().status === "failed"}>
          <summary>
            <Icon name="terminal" />
            <b>Last deploy output</b>
            <span class="muted">revision {d().revision}</span>
            <Show when={d().last_deploy}>
              {(deploy) => (
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={(e) => {
                    e.preventDefault();
                    props.onOpenDeploy(deploy());
                  }}
                >
                  Open deploy
                </Button>
              )}
            </Show>
            <Icon name="down" class="chev" />
          </summary>
          <pre class="term">
            <AnsiText text={d().last_output} />
          </pre>
        </details>
      </Show>
    </div>
  );
}

interface DrawerProps {
  panel: Panel | null;
  canShell: boolean;
  onKind: (kind: PanelKind) => void;
  onClose: () => void;
}

function ContainerDrawer(props: DrawerProps) {
  const running = () => props.panel?.container.state === "running";
  let cache: FileCache = new Map();
  createEffect(() => {
    if (!props.panel) cache = new Map();
  });
  return (
    <Drawer
      show={props.panel != null}
      onClose={props.onClose}
      header={
        <>
          <div class="drawer-title">
            <b>{props.panel?.container.name}</b>
            <span>
              {props.panel?.container.service ?? "app"} · {props.panel?.container.status}
            </span>
          </div>
          <Show when={props.canShell && running()}>
            <SegmentedControl
              label="Panel"
              value={props.panel?.kind ?? "logs"}
              onChange={props.onKind}
              options={PANEL_OPTIONS}
            />
          </Show>
        </>
      }
    >
      <Show when={props.panel} keyed>
        {(p) => (
          <Switch>
            <Match when={p.kind === "logs"}>
              <div class="drawer-sub">
                <Icon name="logs" />
                Live output from the container
              </div>
              <Terminal url={socketUrl(`/containers/${p.container.id}/logs`)} interactive={false} />
            </Match>
            <Match when={p.kind === "shell" && p.exec}>
              {(exec) => (
                <>
                  <div class="drawer-sub">
                    <Icon name="terminal" />
                    New shell session. Closing the panel ends it.
                  </div>
                  <Terminal
                    url={socketUrl(`/exec/${exec()}/ws`)}
                    interactive
                    onResize={(cols, rows) => api.resizeExec(exec(), cols, rows).catch(() => {})}
                  />
                </>
              )}
            </Match>
            <Match when={p.kind === "attach"}>
              <div class="drawer-sub">
                <Icon name="info" />
                Attached to the main process. Only works for containers started with tty and stdin_open.
              </div>
              <Terminal
                url={socketUrl(`/containers/${p.container.id}/attach`)}
                interactive
                onResize={(cols, rows) => api.resizeContainer(p.container.id, cols, rows).catch(() => {})}
              />
            </Match>
            <Match when={p.kind === "files"}>
              <Files container={p.container.id} cache={cache} />
            </Match>
          </Switch>
        )}
      </Show>
    </Drawer>
  );
}
