import { createEffect, createResource, createSignal, For, on, Show } from "solid-js";
import { api, type DeploymentDetail, type DeploySummary } from "../api";
import { commitUrl, deploySeconds, sourceText, triggerText } from "../deploys";
import { formatAgo, formatDuration, formatTime } from "../format";
import { useLive } from "../live";
import { EVENT_DEBOUNCE_MS, eventsConnected, onEvent } from "../states/eventState";
import useToastState from "../states/toastState";
import { DeployOutput, LiveDeploy, StepList } from "./DeployRun";
import { Card, StatusBadge } from "./ui/Blocks";
import Button from "./ui/Button/Button";
import Drawer from "./ui/Drawer/Drawer";
import EmptyState from "./ui/EmptyState/EmptyState";
import { Icon } from "./ui/Icon";
import { ready } from "./ui/Overlay";
import { Busy, SkeletonCardHead, SkeletonList } from "./ui/Skeleton";
import Spinner from "./ui/Spinner/Spinner";
import Tip from "./ui/Tooltip/Tip";

const PAGE = 20;

interface Props {
  detail: DeploymentDetail;
  open?: string;
  onOpen: (deploy: string | undefined) => void;
}

export function DeployHistory(props: Props) {
  const { pushError } = useToastState;
  const [rows, setRows] = createSignal<DeploySummary[]>([]);
  const [more, setMore] = createSignal(false);
  const [loaded, setLoaded] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [loadingMore, setLoadingMore] = createSignal(false);
  let latest = 0;

  /** Reloads as many rows as are shown, so older pages stay open. A slower earlier answer is dropped. */
  async function reload() {
    const call = ++latest;
    try {
      const page = await api.deploys(props.detail.id, 0, Math.max(PAGE, rows().length));
      if (call !== latest) return;
      setRows(page.deploys);
      setMore(page.more);
      setError(null);
    } catch (err) {
      if (call === latest) setError(err instanceof Error ? err.message : String(err));
    } finally {
      if (call === latest) setLoaded(true);
    }
  }

  createEffect(
    on(
      () => props.detail.id,
      () => {
        setRows([]);
        setLoaded(false);
        reload();
      },
    ),
  );
  onEvent((event) => event.kind === "deployment.status" && event.id === props.detail.id, reload, EVENT_DEBOUNCE_MS);
  useLive(() => !eventsConnected() && reload());

  async function loadMore() {
    setLoadingMore(true);
    try {
      const page = await api.deploys(props.detail.id, rows().length, PAGE);
      setRows([...rows(), ...page.deploys]);
      setMore(page.more);
    } catch (err) {
      pushError(err);
    } finally {
      setLoadingMore(false);
    }
  }

  return (
    <>
      <Show
        when={rows().length > 0}
        fallback={
          <Show
            when={loaded()}
            fallback={
              <Busy>
                <section class="card">
                  <SkeletonCardHead />
                  <SkeletonList rows={3} icon={false} actions={0} />
                </section>
              </Busy>
            }
          >
            <Card>
              <EmptyState
                icon="rotate"
                title={error() ? "Couldn't load the deploys" : "No deploys yet"}
                description={error() ?? "Each deploy shows up here with its output."}
              />
            </Card>
          </Show>
        }
      >
        <Card title="Deploys" description="Newest first. Click one to see its steps and output.">
          <div class="deploy-table">
            <table class="tbl">
              <thead>
                <tr>
                  <th>Revision</th>
                  <th>Status</th>
                  <th>Started by</th>
                  <th class="hide-sm">Source</th>
                  <th>Started</th>
                  <th class="num">Took</th>
                </tr>
              </thead>
              <tbody>
                <For each={rows()}>{(d) => <DeployRow deploy={d} onOpen={props.onOpen} />}</For>
              </tbody>
            </table>
          </div>
          <Show when={more()}>
            <div class="card-f" style={{ "justify-content": "center" }}>
              <Button size="sm" disabled={loadingMore()} onClick={loadMore}>
                Show older deploys
              </Button>
            </div>
          </Show>
        </Card>
      </Show>
      <DeployDrawer detail={props.detail} deploy={props.open} onClose={() => props.onOpen(undefined)} />
    </>
  );
}

function DeployRow(props: { deploy: DeploySummary; onOpen: (deploy: string) => void }) {
  const d = () => props.deploy;
  return (
    <tr class="click" onClick={() => props.onOpen(d().id)}>
      <td class="mono">{d().revision}</td>
      <td>
        <StatusBadge status={d().status} />
      </td>
      <td>{triggerText(d())}</td>
      <td class="hide-sm">
        <Show when={commitUrl(d().source)} fallback={<span class="mono">{sourceText(d().source)}</span>}>
          {(href) => (
            <a class="link mono" href={href()} target="_blank" rel="noreferrer" onClick={(e) => e.stopPropagation()}>
              {sourceText(d().source)}
              <Icon name="ext" />
            </a>
          )}
        </Show>
      </td>
      <td>
        <Tip content={`${formatTime(d().started_at)} UTC`}>
          <span>{formatAgo(d().started_at)}</span>
        </Tip>
      </td>
      <td class="num">
        <Show when={d().finished_at != null} fallback={<Spinner size={14} />}>
          {formatDuration(deploySeconds(d(), Date.now() / 1000))}
        </Show>
      </td>
    </tr>
  );
}

function DeployDrawer(props: { detail: DeploymentDetail; deploy?: string; onClose: () => void }) {
  const [found, { refetch }] = createResource(
    () => (props.deploy ? { id: props.detail.id, deploy: props.deploy } : undefined),
    (key) => api.deployDetail(key.id, key.deploy),
  );
  onEvent((event) => event.kind === "deployment.status" && event.id === props.detail.id, refetch, EVENT_DEBOUNCE_MS);
  const shown = () => (props.deploy ? (ready(found) ?? found.latest) : undefined);

  return (
    <Drawer
      show={props.deploy != null}
      onClose={props.onClose}
      header={
        <div class="drawer-title">
          <b>Revision {shown()?.revision ?? ""}</b>
          <span>
            <Show when={shown()}>{(d) => `${triggerText(d())} · ${formatTime(d().started_at)} UTC`}</Show>
          </span>
        </div>
      }
    >
      <Show
        when={shown()}
        fallback={
          <Show
            when={found.error}
            fallback={
              <div class="drawer-sub">
                <Spinner size={14} />
                Loading
              </div>
            }
          >
            <EmptyState
              icon="alert"
              tone="danger"
              title="Couldn't load this deploy"
              description={String(found.error?.message)}
            />
          </Show>
        }
      >
        {(d) => (
          <div class="deploy-view">
            <div class="hrow">
              <StatusBadge status={d().status} />
              <span class="mono muted">{sourceText(d().source)}</span>
            </div>
            <Show
              when={d().status === "deploying"}
              fallback={
                <>
                  <Show when={d().steps.length > 0}>
                    <StepList steps={d().steps} />
                  </Show>
                  <Show when={d().output} fallback={<p class="muted">This deploy wrote no output.</p>}>
                    <DeployOutput text={d().output} height="none" />
                  </Show>
                </>
              }
            >
              <LiveDeploy deployment={props.detail.id} onEnd={refetch} />
            </Show>
          </div>
        )}
      </Show>
    </Drawer>
  );
}
