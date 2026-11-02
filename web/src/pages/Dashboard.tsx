import { A } from "@solidjs/router";
import { createResource, For, type JSX, Show } from "solid-js";
import { useUser } from "../App";
import { type Attention, api, can, type DaemonInfo, type Deployment, type Status } from "../api";
import { Card, Chevron, Note, PageHeader, Pill, StatusBadge } from "../components/ui/Blocks";
import Button from "../components/ui/Button/Button";
import EmptyState from "../components/ui/EmptyState/EmptyState";
import { Icon, type IconName } from "../components/ui/Icon";
import { ready } from "../components/ui/Overlay";
import {
  Bone,
  Busy,
  SkeletonCardHead,
  SkeletonDetails,
  SkeletonEmpty,
  SkeletonList,
  SkeletonStats,
} from "../components/ui/Skeleton";
import Tip from "../components/ui/Tooltip/Tip";
import { formatAgo, formatBytes } from "../format";
import { useLive } from "../live";
import { rowsFor, useShape } from "../shape";
import { EVENT_DEBOUNCE_MS, eventsConnected, isDeploymentEvent, onEvent } from "../states/eventState";

const ROWS: [string, (info: DaemonInfo) => string | number | null][] = [
  ["Engine", (i) => i.server_version],
  ["API", (i) => i.api_version],
  ["OS", (i) => i.operating_system],
  ["Architecture", (i) => i.architecture],
  ["CPUs", (i) => i.cpus],
  ["Memory", (i) => (i.memory_bytes == null ? null : formatBytes(i.memory_bytes))],
  ["Containers", (i) => (i.containers == null ? null : `${i.containers_running ?? 0} running of ${i.containers}`)],
  ["Images", (i) => i.images],
  ["Swarm", (i) => i.swarm],
];

const ATTENTION: Status[] = ["failed", "down", "degraded", "deploying"];
const STATUS_ORDER: Status[] = ["running", "deploying", "degraded", "down", "failed", "stopped", "created"];

const ATTENTION_TEXT: Record<string, string> = {
  failed: "Last deploy failed",
  down: "No container is running",
  degraded: "Some containers stopped",
  deploying: "Deploying a new revision",
};

interface PoolLayer {
  label: string;
  value: number;
  hint: string;
}

// `used` is what is actually in use right now. `outer` is the wider amount drawn behind it: what the pool check counts
// for CPU and memory, the whole disk for storage.
const POOL_HINT = {
  used: "What running containers use right now, from the last sample.",
  counted: "Peak use over the last 5 minutes. New deploys are checked against this plus their own limits.",
};

function PoolCard(props: {
  label: string;
  icon: IconName;
  used: PoolLayer;
  outer: PoolLayer | null;
  total: number | null;
  totalLabel: string;
  format: (v: number) => string;
  note?: string;
}) {
  const percent = (value: number) => (props.total ? (value / props.total) * 100 : 0);
  const shown = (value: number) => `${Math.min(100, percent(value))}%`;
  const hot = () => Math.max(percent(props.used.value), percent(props.outer?.value ?? 0)) > 80;
  return (
    <Card class="stat">
      <div class="k">
        <Icon name={props.icon} />
        {props.label}
      </div>
      <div class="v">
        {props.format(props.used.value)} <small>{props.totalLabel}</small>
      </div>
      <Show when={props.total}>
        <div class={hot() ? "meter hot" : "meter"}>
          <Show when={props.outer}>{(outer) => <span class="soft" style={{ width: shown(outer().value) }} />}</Show>
          <span style={{ width: shown(props.used.value) }} />
        </div>
      </Show>
      <div class="pool-legend">
        <Tip content={props.used.hint}>
          <span>
            <i />
            {props.used.label}
            <Show when={props.total}> {Math.round(percent(props.used.value))}%</Show>
          </span>
        </Tip>
        <Show when={props.outer}>
          {(outer) => (
            <Tip content={outer().hint}>
              <span>
                <i class="soft" />
                {outer().label} {props.format(outer().value)}
              </span>
            </Tip>
          )}
        </Show>
      </div>
      <Show when={props.note}>
        <div class="pool-note">{props.note}</div>
      </Show>
    </Card>
  );
}

function withoutLimit(count: number, kind: string) {
  if (count === 0) return undefined;
  return `${count} ${count === 1 ? "container has" : "containers have"} no ${kind} limit and can use whatever is free.`;
}

function DeploymentsCard(props: { deployments: Deployment[]; proxy: Attention[] }) {
  const attention = () => props.deployments.filter((d) => ATTENTION.includes(d.status));
  const counts = () =>
    STATUS_ORDER.map((s) => [s, props.deployments.filter((d) => d.status === s).length] as const).filter(([, n]) => n);
  return (
    <Card
      title="Deployments"
      description={`${props.deployments.length} total`}
      actions={
        <Button size="sm" href="/deployments">
          View all
        </Button>
      }
    >
      <For each={props.proxy}>
        {(item) => (
          <A class="list-i" href={item.href}>
            <span class="dot warn" />
            <div class="nm grow">
              <b>{item.message}</b>
              <span class="plain">{item.kind === "certificate" ? "Replace it on the Identities page" : "Proxy"}</span>
            </div>
            <Chevron />
          </A>
        )}
      </For>
      <Show
        when={props.deployments.length > 0}
        fallback={<EmptyState compact icon="box" title="No deployments yet" description="Create one to see it here." />}
      >
        <div class="card-b hrow wrap" style={{ gap: "6px", "border-bottom": "1px solid var(--line-soft)" }}>
          <For each={counts()}>
            {([status, n]) => (
              <span class={`badge s-${status}`}>
                <i />
                {n} {status}
              </span>
            )}
          </For>
        </div>
        <Show
          when={attention().length > 0}
          fallback={
            <Show when={props.proxy.length === 0}>
              <EmptyState compact icon="check" tone="success" title="All good" description="Nothing needs a look." />
            </Show>
          }
        >
          <For each={attention()}>
            {(d) => (
              <A class="list-i" href={`/deployments/${d.id}`}>
                <span class={d.status === "deploying" || d.status === "degraded" ? "dot warn" : "dot bad"} />
                <div class="nm grow">
                  <b>{d.name}</b>
                  <span class="plain">
                    {ATTENTION_TEXT[d.status]} · {formatAgo(d.updated_at)}
                  </span>
                </div>
                <StatusBadge status={d.status} />
                <Chevron />
              </A>
            )}
          </For>
        </Show>
      </Show>
    </Card>
  );
}

export function Dashboard() {
  const user = useUser();
  const [info] = createResource(api.dockerInfo);
  const [pool, { refetch: reloadPool }] = createResource(api.pool);
  const [deployments, { refetch: reloadDeployments }] = createResource(api.deployments);
  const [proxyAttention, { refetch: reloadAttention }] = createResource(api.proxyAttention);
  useLive(() =>
    Promise.all([reloadPool(), !eventsConnected() && reloadDeployments(), !eventsConnected() && reloadAttention()]),
  );
  onEvent(isDeploymentEvent, reloadDeployments, EVENT_DEBOUNCE_MS);
  onEvent((event) => isDeploymentEvent(event) || event.kind === "proxy.traefik", reloadAttention, EVENT_DEBOUNCE_MS);
  const listShape = useShape("dashboard-deployments", { total: 1, attention: 0 }, () => {
    const list = deployments.latest;
    const proxy = proxyAttention.latest;
    if (!list || !proxy) return undefined;
    return { total: list.length, attention: list.filter((d) => ATTENTION.includes(d.status)).length + proxy.length };
  });
  const poolNote = useShape("dashboard-pool-note", false, () => {
    const p = pool.latest;
    return p ? p.cpus_unlimited > 0 || p.memory_unlimited > 0 : undefined;
  });

  const failed = (error: unknown, skeleton: JSX.Element): JSX.Element => (
    <Show when={error} fallback={<Busy>{skeleton}</Busy>}>
      <Card>
        <EmptyState compact icon="alert" tone="danger" title="Couldn't load this" description={String(error)} />
      </Card>
    </Show>
  );

  return (
    <div class="content">
      <PageHeader
        title="Dashboard"
        description="Overview of this server"
        actions={
          <Show when={can(user(), "dep.create")}>
            <Button variant="primary" href="/deployments/new">
              <Icon name="plus" />
              New deployment
            </Button>
          </Show>
        }
      />
      <div class="stack">
        <Show when={ready(pool)} fallback={failed(pool.error?.message, <SkeletonStats count={3} note={poolNote} />)}>
          {(p) => (
            <div class="grid3">
              <PoolCard
                label="CPU"
                icon="cpu"
                used={{ label: "In use", value: p().cpus_actual, hint: POOL_HINT.used }}
                outer={{ label: "Counted", value: p().cpus_counted, hint: POOL_HINT.counted }}
                total={p().cpus}
                totalLabel={`of ${p().cpus.toFixed(2)} CPUs`}
                format={(v) => `${v.toFixed(2)}`}
                note={withoutLimit(p().cpus_unlimited, "CPU")}
              />
              <PoolCard
                label="Memory"
                icon="mem"
                used={{ label: "In use", value: p().memory_actual, hint: POOL_HINT.used }}
                outer={{ label: "Counted", value: p().memory_counted, hint: POOL_HINT.counted }}
                total={p().memory_bytes}
                totalLabel={`of ${formatBytes(p().memory_bytes)}`}
                format={formatBytes}
                note={withoutLimit(p().memory_unlimited, "memory")}
              />
              <PoolCard
                label="Storage"
                icon="db"
                used={{
                  label: "kuroe and Docker",
                  value: p().storage_used,
                  hint: "Images, build cache and kuroe's volumes.",
                }}
                outer={
                  p().disk_used == null
                    ? null
                    : { label: "Whole disk", value: p().disk_used ?? 0, hint: "Everything on the data disk." }
                }
                total={p().storage_bytes ?? p().disk_total}
                totalLabel={
                  p().storage_bytes != null
                    ? `of ${formatBytes(p().storage_bytes ?? 0)} pool`
                    : p().disk_total != null
                      ? `of ${formatBytes(p().disk_total ?? 0)} disk`
                      : "used, no limit"
                }
                format={formatBytes}
              />
            </div>
          )}
        </Show>
        <div class="split">
          <Show
            when={ready(deployments)}
            fallback={failed(
              deployments.error?.message,
              <section class="card">
                <SkeletonCardHead description action />
                <Show when={listShape.total > 0} fallback={<SkeletonEmpty compact />}>
                  <div class="card-b hrow wrap" style={{ gap: "6px", "border-bottom": "1px solid var(--line-soft)" }}>
                    <Bone w="76px" size="pill" />
                    <Bone w="64px" size="pill" />
                  </div>
                  <Show when={listShape.attention > 0} fallback={<SkeletonEmpty compact />}>
                    <SkeletonList bare rows={rowsFor(listShape.attention, 6)} icon={false} actions={1} />
                  </Show>
                </Show>
              </section>,
            )}
          >
            {(list) => <DeploymentsCard deployments={list()} proxy={ready(proxyAttention) ?? []} />}
          </Show>
          <Card
            title="Docker host"
            actions={
              <Show
                when={ready(info)}
                fallback={
                  <Show when={info.error}>
                    <Pill tone="bad" dot>
                      unreachable
                    </Pill>
                  </Show>
                }
              >
                <Pill tone="ok" dot>
                  connected
                </Pill>
              </Show>
            }
          >
            <Show
              when={ready(info)}
              fallback={
                <Show
                  when={info.error}
                  fallback={
                    <Busy>
                      <SkeletonDetails rows={ROWS.length} />
                    </Busy>
                  }
                >
                  <div class="card-b">
                    <Note tone="bad">{String(info.error?.message ?? info.error)}</Note>
                  </div>
                </Show>
              }
            >
              {(data) => (
                <dl class="dl">
                  <For each={ROWS}>
                    {([label, value]) => (
                      <>
                        <dt>{label}</dt>
                        <dd>{value(data()) ?? "-"}</dd>
                      </>
                    )}
                  </For>
                </dl>
              )}
            </Show>
          </Card>
        </div>
      </div>
    </div>
  );
}
