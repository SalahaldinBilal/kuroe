import { createResource, createSignal, For, Show } from "solid-js";
import { useUser } from "../App";
import { api, can, type HostPoint, type NetworkPoint } from "../api";
import { gapFor, LineChart } from "../components/LineChart";
import { isLive, type Range, RangePicker, rangeDomain, windowOf } from "../components/RangePicker";
import { Card, Note, PageHeader } from "../components/ui/Blocks";
import Button from "../components/ui/Button/Button";
import EmptyState from "../components/ui/EmptyState/EmptyState";
import { Icon } from "../components/ui/Icon";
import { ready } from "../components/ui/Overlay";
import { Bone, Busy } from "../components/ui/Skeleton";
import { formatBytes, formatPercent, formatRate, formatTime } from "../format";
import { useLive } from "../live";
import useToastState from "../states/toastState";

const HOST_SAMPLE_SECONDS = 30;
const CONTAINER_SAMPLE_SECONDS = 10;
const NETWORK_HINT =
  "All deployment containers together, including traffic between them and from the proxy. kuroe's own containers aren't counted.";

const STORAGE: [string, string, string][] = [
  ["kuroe_db", "kuroe database", "var(--accent)"],
  ["kuroe_logs", "kuroe logs", "color-mix(in srgb, var(--accent) 60%, var(--surface))"],
  ["kuroe_other", "kuroe other files", "color-mix(in srgb, var(--accent) 30%, var(--surface))"],
  ["docker_images", "Docker images", "var(--ink)"],
  ["docker_containers", "Docker containers", "color-mix(in srgb, var(--ink) 65%, var(--surface))"],
  ["docker_volumes", "Docker volumes", "color-mix(in srgb, var(--ink) 42%, var(--surface))"],
  ["docker_build_cache", "Docker build cache", "color-mix(in srgb, var(--ink) 22%, var(--surface))"],
];

export function Metrics() {
  const user = useUser();
  const { pushToast, pushError } = useToastState;
  const [range, setRange] = createSignal<Range>({ preset: "24h" });
  const [host, { refetch: reloadHost }] = createResource(range, (r) => api.hostMetrics(windowOf(r)));
  const [storage, { mutate, refetch: reloadStorage }] = createResource(api.storage);
  const quiet = useLive(() => isLive(range()) && reloadHost(), HOST_SAMPLE_SECONDS);
  useLive(reloadStorage, 60);
  const [info] = createResource(api.dockerInfo);
  const cores = () => ready(info)?.cpus;
  const [measuring, setMeasuring] = createSignal(false);
  const canRefresh = () => can(user(), "settings");

  const series = (pick: (p: HostPoint) => number | null) =>
    (host.latest?.points ?? []).map((p) => ({ t: p.t, v: pick(p) }));
  const network = (pick: (p: NetworkPoint) => number) =>
    (host.latest?.network ?? []).map((p) => ({ t: p.t, v: pick(p) }));
  const networkAxis = () => ({ ...axis(), gap: gapFor(host.latest?.bucket ?? 0, CONTAINER_SAMPLE_SECONDS) });
  const lastPoint = () => host.latest?.points.at(-1);
  const axis = () => ({
    gap: gapFor(host.latest?.bucket ?? 0, HOST_SAMPLE_SECONDS),
    domain: rangeDomain(range()),
    loading: !host.latest && !host.error,
  });
  const sizes = () =>
    STORAGE.map(([key, label, color]) => ({ label, color, bytes: ready(storage)?.categories[key] ?? 0 }));
  const total = () => sizes().reduce((sum, s) => sum + s.bytes, 0);

  async function refresh() {
    setMeasuring(true);
    try {
      mutate(await api.refreshStorage());
      pushToast("Storage measured");
    } catch (err) {
      pushError(err);
    } finally {
      setMeasuring(false);
    }
  }

  return (
    <div class="content">
      <PageHeader
        title="Metrics"
        description="This host and its disk"
        actions={<RangePicker value={range()} onChange={setRange} />}
      />
      <div class="stack" style={{ gap: "24px" }}>
        <Show when={host.error}>
          <Note tone="bad">{String(host.error?.message ?? host.error)}</Note>
        </Show>
        <div class="charts wide" classList={{ refetching: host.loading && !quiet() }}>
          <LineChart {...axis()} title="CPU" points={series((p) => p.cpu)} format={formatPercent} max={100} />
          <LineChart
            {...axis()}
            title="Memory"
            points={series((p) => p.mem)}
            format={formatBytes}
            limit={lastPoint() ? { value: lastPoint()?.mem_total ?? 0, label: "total" } : undefined}
          />
          <LineChart
            {...axis()}
            title="Disk used, data dir"
            points={series((p) => p.disk)}
            format={formatBytes}
            limit={lastPoint() ? { value: lastPoint()?.disk_total ?? 0, label: "size" } : undefined}
          />
          <LineChart
            {...axis()}
            title="CPU demand (load average)"
            hint="Processes running or waiting for a CPU, averaged over a minute. Below the line, cores are free. Above it, work waits for a CPU. High demand with low CPU usually means a slow disk."
            points={series((p) => p.load1)}
            format={(v) => (cores() ? `${v.toFixed(2)} of ${cores()} cores` : v.toFixed(2))}
            limit={cores() ? { value: cores() ?? 0, label: "cores", text: "all cores busy" } : undefined}
          />
          <LineChart
            {...networkAxis()}
            title="Network in, deployments"
            hint={NETWORK_HINT}
            points={network((p) => p.rx_rate)}
            format={formatRate}
          />
          <LineChart
            {...networkAxis()}
            title="Network out, deployments"
            hint={NETWORK_HINT}
            points={network((p) => p.tx_rate)}
            format={formatRate}
          />
        </div>

        <Card
          title={
            <>
              Storage{" "}
              <Show when={ready(storage)?.ts}>
                <span class="muted" style={{ "font-weight": "400", "margin-left": "6px" }}>
                  {formatBytes(total())}
                </span>
              </Show>
            </>
          }
          description={
            <Show when={ready(storage)?.ts} fallback="kuroe measures every 10 minutes.">
              {(ts) => `Measured ${formatTime(ts())} UTC. kuroe measures every 10 minutes.`}
            </Show>
          }
          actions={
            <Show when={canRefresh()}>
              <Button size="sm" disabled={measuring()} onClick={refresh}>
                <Icon name="rotate" />
                Measure now
              </Button>
            </Show>
          }
        >
          <Show
            when={ready(storage)?.ts}
            fallback={
              <Show
                when={storage.latest || storage.error}
                fallback={
                  <Busy>
                    <div class="card-b">
                      <span class="bone stackbar" />
                    </div>
                    <div class="legend grid2">
                      <For each={STORAGE}>
                        {() => (
                          <div class="list-i">
                            <Bone w="45%" />
                          </div>
                        )}
                      </For>
                    </div>
                  </Busy>
                }
              >
                <EmptyState compact icon="db" title="Not measured yet" description="Check back in a few minutes." />
              </Show>
            }
          >
            <div class="card-b">
              <div class="stackbar">
                <For each={sizes()}>
                  {(s) => (
                    <Show when={s.bytes > 0}>
                      <span
                        style={{ width: `${(s.bytes / Math.max(total(), 1)) * 100}%`, background: s.color }}
                        title={`${s.label}: ${formatBytes(s.bytes)}`}
                      />
                    </Show>
                  )}
                </For>
              </div>
            </div>
            <div class="legend grid2">
              <For each={sizes()}>
                {(s) => (
                  <div class="list-i">
                    <span class="sw-c" style={{ background: s.color }} />
                    <span class="grow">{s.label}</span>
                    <span class="mono muted">{formatBytes(s.bytes)}</span>
                    <span class="mono faint" style={{ width: "44px", "text-align": "right" }}>
                      {((s.bytes / Math.max(total(), 1)) * 100).toFixed(0)}%
                    </span>
                  </div>
                )}
              </For>
            </div>
          </Show>
        </Card>
      </div>
    </div>
  );
}
