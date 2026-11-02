import { createResource, createSignal, Show } from "solid-js";
import { api, type ServicePoint, type ServiceSettings } from "../api";
import { formatBytes, formatPercent, formatRate } from "../format";
import { useLive } from "../live";
import { gapFor, LineChart } from "./LineChart";
import { isLive, type Range, RangePicker, rangeDomain, windowOf } from "./RangePicker";
import { Card, Note } from "./ui/Blocks";
import EmptyState from "./ui/EmptyState/EmptyState";
import { ready } from "./ui/Overlay";
import Select, { items } from "./ui/Select/Select";
import { Busy, SkeletonCharts } from "./ui/Skeleton";

const CONTAINER_SAMPLE_SECONDS = 10;

interface Props {
  deployment: string;
  services: Record<string, ServiceSettings>;
  stack: boolean;
}

const BYTES_PER_MBIT = 125_000;

export function DeploymentMetrics(props: Props) {
  const [range, setRange] = createSignal<Range>({ preset: "1h" });
  const [picked, setPicked] = createSignal<string | null>(null);
  const [data, { refetch }] = createResource(
    () => ({ id: props.deployment, range: range() }),
    ({ id, range }) => api.deploymentMetrics(id, windowOf(range)),
  );
  const quiet = useLive(() => isLive(range()) && refetch(), CONTAINER_SAMPLE_SECONDS);
  const services = () => Object.keys(data.latest?.services ?? {}).sort();
  const service = () => {
    const p = picked();
    return p && services().includes(p) ? p : services()[0];
  };
  const points = () => data.latest?.services[service() ?? ""] ?? [];
  const pick = (f: (p: ServicePoint) => number) => points().map((p) => ({ t: p.t, v: f(p) }));
  const memLimit = () => points().at(-1)?.mem_limit;
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
        <span class="grow" />
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
        <div class="charts" classList={{ refetching: data.loading && !quiet() }}>
          <LineChart {...axis()} title="CPU" points={pick((p) => p.cpu)} format={formatPercent} />
          <LineChart
            {...axis()}
            title="Memory"
            points={pick((p) => p.mem)}
            format={formatBytes}
            limit={memLimit() ? { value: memLimit() ?? 0, label: "limit" } : undefined}
          />
          <LineChart
            {...axis()}
            title="Network in"
            points={pick((p) => p.rx_rate)}
            format={formatRate}
            limit={netLimit("net_in_mbit", "default_net_in_mbit")}
          />
          <LineChart
            {...axis()}
            title="Network out"
            points={pick((p) => p.tx_rate)}
            format={formatRate}
            limit={netLimit("net_out_mbit", "default_net_out_mbit")}
          />
          <LineChart {...axis()} title="Disk read" points={pick((p) => p.read_rate)} format={formatRate} />
          <LineChart {...axis()} title="Disk write" points={pick((p) => p.write_rate)} format={formatRate} />
        </div>
      </Show>
    </>
  );
}
