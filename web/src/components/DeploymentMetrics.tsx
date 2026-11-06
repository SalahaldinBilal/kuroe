import { createMemo, createResource, createSignal, Show } from "solid-js";
import { api, type ServicePoint, type ServiceSettings } from "../api";
import { formatBytes, formatPercent, formatRate } from "../format";
import { useLive } from "../live";
import { gapFor, LineChart, type Series, topSeries } from "./LineChart";
import { isLive, LiveStatus, type Range, RangePicker, rangeDomain, windowOf } from "./RangePicker";
import { Card, Note } from "./ui/Blocks";
import EmptyState from "./ui/EmptyState/EmptyState";
import { ready } from "./ui/Overlay";
import SegmentedControl from "./ui/SegmentedControl/SegmentedControl";
import Select, { items } from "./ui/Select/Select";
import { Busy, SkeletonCharts } from "./ui/Skeleton";

const CONTAINER_SAMPLE_SECONDS = 10;
const BYTES_PER_MBIT = 125_000;

interface Props {
  deployment: string;
  services: Record<string, ServiceSettings>;
  stack: boolean;
  /** Running containers, to name the lines by container. */
  containers: { id: string; name: string }[];
}

type View = "total" | "containers";

export function DeploymentMetrics(props: Props) {
  const [range, setRange] = createSignal<Range>({ preset: "1h" });
  const [view, setView] = createSignal<View>("total");
  const [picked, setPicked] = createSignal<string | null>(null);
  const deployment = createMemo(() => props.deployment);
  const [data, { refetch }] = createResource(
    () => ({ id: deployment(), range: range() }),
    ({ id, range }) => api.deploymentMetrics(id, windowOf(range)),
  );
  const quiet = useLive(() => isLive(range()) && refetch(), CONTAINER_SAMPLE_SECONDS);
  const services = () => Object.keys(data.latest?.services ?? {}).sort();
  const service = () => {
    const p = picked();
    return p && services().includes(p) ? p : services()[0];
  };
  const byContainer = () => view() === "containers";
  const points = () => data.latest?.services[service() ?? ""] ?? [];
  const containers = () => data.latest?.containers ?? [];

  function label(c: { service: string; container_id: string }): string {
    const running = props.containers.find((r) => r.id.startsWith(c.container_id) || c.container_id.startsWith(r.id));
    return running?.name ?? `${c.service} · ${c.container_id.slice(0, 12)}`;
  }

  function chart(f: (p: ServicePoint) => number): { points: { t: number; v: number }[] } | { series: Series[] } {
    if (!byContainer()) return { points: points().map((p) => ({ t: p.t, v: f(p) })) };
    const series = containers().map((c) => ({ label: label(c), points: c.points.map((p) => ({ t: p.t, v: f(p) })) }));
    return { series: topSeries(series) };
  }

  const [info] = createResource(api.dockerInfo);
  /** Docker reports the host's memory as the limit of a container that has none. */
  const realLimit = (bytes: number | undefined) => {
    const host = ready(info)?.memory_bytes;
    return bytes && host && bytes < host ? bytes : undefined;
  };
  const memLimit = () => {
    if (!byContainer()) return realLimit(points().at(-1)?.mem_limit);
    const limits = new Set(containers().map((c) => realLimit(c.points.at(-1)?.mem_limit)));
    return limits.size === 1 ? [...limits][0] : undefined;
  };
  const [settings] = createResource(api.settings);
  function netLimit(own: "net_in_mbit" | "net_out_mbit", fallback: string) {
    if (props.stack) return undefined;
    const mbit = props.services[service() ?? ""]?.[own] ?? Number(ready(settings)?.[fallback]);
    return mbit > 0 ? { value: mbit * BYTES_PER_MBIT, label: "limit per container" } : undefined;
  }
  const axis = () => ({
    gap: gapFor(data.latest?.bucket ?? 0, CONTAINER_SAMPLE_SECONDS),
    domain: rangeDomain(range()),
  });

  return (
    <>
      <div class="hrow wrap" style={{ "margin-bottom": "14px", gap: "10px" }}>
        <SegmentedControl
          label="Show"
          value={view()}
          onChange={setView}
          options={[
            { value: "total" as const, label: "Total" },
            { value: "containers" as const, label: "By container" },
          ]}
        />
        <Show when={!byContainer()}>
          <Show
            when={services().length > 1}
            fallback={<Show when={service()}>{(s) => <span class="chip">{s()}</span>}</Show>}
          >
            <Select
              mono
              label="Service"
              value={service() ?? ""}
              onChange={setPicked}
              items={items(services())}
              style={{ "min-width": "160px" }}
            />
          </Show>
        </Show>
        <span class="grow" />
        <Show when={isLive(range())}>
          <LiveStatus refreshing={quiet()} />
        </Show>
        <RangePicker value={range()} onChange={setRange} />
      </div>
      <Show when={data.error}>
        <Note tone="bad">{String(data.error?.message ?? data.error)}</Note>
      </Show>
      <Show
        when={services().length > 0}
        fallback={
          <Show
            when={data.latest || data.error}
            fallback={
              <Busy>
                <SkeletonCharts count={6} />
              </Busy>
            }
          >
            <Card>
              <EmptyState
                icon="activity"
                title="No samples yet"
                description="kuroe samples every 10 seconds once a container is running."
              />
            </Card>
          </Show>
        }
      >
        <div class="charts" classList={{ wide: byContainer(), refetching: data.loading && !quiet() }}>
          <LineChart {...axis()} {...chart((p) => p.cpu)} title="CPU" format={formatPercent} />
          <LineChart
            {...axis()}
            {...chart((p) => p.mem)}
            title="Memory"
            binary
            format={formatBytes}
            limit={
              memLimit()
                ? { value: memLimit() ?? 0, label: byContainer() ? "limit per container" : "limit" }
                : undefined
            }
          />
          <LineChart
            {...axis()}
            {...chart((p) => p.rx_rate)}
            title="Network in"
            binary
            format={formatRate}
            limit={netLimit("net_in_mbit", "default_net_in_mbit")}
          />
          <LineChart
            {...axis()}
            {...chart((p) => p.tx_rate)}
            title="Network out"
            binary
            format={formatRate}
            limit={netLimit("net_out_mbit", "default_net_out_mbit")}
          />
          <LineChart {...axis()} {...chart((p) => p.read_rate)} title="Disk read" binary format={formatRate} />
          <LineChart {...axis()} {...chart((p) => p.write_rate)} title="Disk write" binary format={formatRate} />
        </div>
      </Show>
    </>
  );
}
