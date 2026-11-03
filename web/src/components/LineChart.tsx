import { createMemo, createSignal, createUniqueId, For, Show } from "solid-js";
import EmptyState from "./ui/EmptyState/EmptyState";

export interface Point {
  t: number;
  v: number | null;
}

export interface Series {
  label: string;
  points: Point[];
}

interface Props {
  title: string;
  /** One line under the title saying how to read the chart. */
  hint?: string;
  points?: Point[];
  /** Several lines with a legend, in place of `points`. */
  series?: Series[];
  format: (v: number) => string;
  /** A shorter format for the axis labels. */
  tick?: (v: number) => string;
  /** Steps in powers of two, for bytes. */
  binary?: boolean;
  /** A reference line. `text` replaces the default "label value" next to it. */
  limit?: { value: number; label: string; text?: string };
  gap?: number;
  domain?: [number, number];
  /** Shows a placeholder of the chart's size until the first data arrives. */
  loading?: boolean;
}

type Sample = { t: number; v: number };

const WIDTH = 520;
const HEIGHT = 150;
const PAD = { top: 8, right: 6, bottom: 20, left: 64 };
/** A limit joins the scale once the data reaches this share of it. Above that it would flatten the line. */
const LIMIT_REACH = 0.5;
const SERIES_COLORS = 8;
export const OTHER = "other";

export function gapFor(bucket: number, sampleEvery: number): number {
  return Math.max(bucket, sampleEvery) * 2.5;
}

export function runs(points: Point[], gap: number): Sample[][] {
  const out: Sample[][] = [];
  let current: Sample[] = [];
  let prev: number | null = null;
  for (const p of points) {
    const broken = p.v == null || (prev != null && p.t - prev > gap);
    if (broken && current.length) {
      out.push(current);
      current = [];
    }
    if (p.v != null) {
      current.push({ t: p.t, v: p.v });
      prev = p.t;
    }
  }
  if (current.length) out.push(current);
  return out;
}

function steps(peak: number, binary: boolean): number[] {
  const out: number[] = [];
  if (binary) {
    const low = Math.floor(Math.log2(peak / 8));
    for (let k = low; k <= low + 4; k++) out.push(2 ** k, 1.5 * 2 ** k);
  } else {
    const low = Math.floor(Math.log10(peak / 8));
    for (let k = low; k <= low + 2; k++) for (const m of [1, 2, 5, 2.5]) out.push(m * 10 ** k);
  }
  return out;
}

/** The top and gridlines closest above `peak`, with three to six steps. */
export function yAxis(peak: number, binary = false): { top: number; ticks: number[] } {
  const target = peak > 0 ? peak : binary ? 1024 : 1;
  let best: { top: number; step: number; n: number } | null = null;
  for (const step of steps(target, binary)) {
    const n = Math.ceil(target / step - 1e-9);
    if (n < 3 || n > 6) continue;
    const top = n * step;
    if (!best || top < best.top - 1e-9) best = { top, step, n };
  }
  const { top, step, n } = best ?? { top: target, step: target / 4, n: 4 };
  return { top, ticks: Array.from({ length: n + 1 }, (_, i) => i * step) };
}

/** The seven series with the highest peaks, the rest added up as "other". */
export function topSeries(series: Series[], keep = SERIES_COLORS - 1): Series[] {
  if (series.length <= keep + 1) return series;
  const peak = (s: Series) => Math.max(0, ...s.points.map((p) => p.v ?? 0));
  const sorted = [...series].sort((a, b) => peak(b) - peak(a));
  const rest = new Map<number, number>();
  for (const s of sorted.slice(keep)) {
    for (const p of s.points) if (p.v != null) rest.set(p.t, (rest.get(p.t) ?? 0) + p.v);
  }
  const other = [...rest].sort(([a], [b]) => a - b).map(([t, v]) => ({ t, v }));
  return [...sorted.slice(0, keep), { label: OTHER, points: other }];
}

export function seriesColor(label: string, i: number): string {
  return label === OTHER ? "var(--faint)" : `var(--series-${(i % SERIES_COLORS) + 1})`;
}

function niceTime(t: number, span: number): string {
  const d = new Date(t * 1000);
  if (span <= 2 * 86_400) return d.toISOString().slice(11, 16);
  return d.toISOString().slice(5, 10);
}

function stamp(t: number): string {
  return new Date(t * 1000).toISOString().slice(5, 16).replace("T", " ");
}

export function LineChart(props: Props) {
  const gradient = createUniqueId();
  const [hover, setHover] = createSignal<number | null>(null);
  const multi = () => props.series !== undefined;
  const lines = createMemo<Series[]>(() => props.series ?? [{ label: props.title, points: props.points ?? [] }]);
  const samples = createMemo(() => lines().map((s) => s.points.filter((p): p is Sample => p.v != null)));
  const times = createMemo(() => [...new Set(samples().flatMap((s) => s.map((p) => p.t)))].sort((a, b) => a - b));
  const segments = createMemo(() => lines().map((s) => runs(s.points, props.gap ?? Number.POSITIVE_INFINITY)));
  const peak = () => Math.max(0, ...samples().flatMap((s) => s.map((p) => p.v)));
  const limitShown = () => {
    const limit = props.limit;
    return !!limit && limit.value > 0 && peak() >= limit.value * LIMIT_REACH;
  };

  const scale = createMemo(() => {
    const t0 = props.domain?.[0] ?? times()[0] ?? 0;
    const t1 = props.domain?.[1] ?? times()[times().length - 1] ?? t0 + 1;
    const axis = yAxis(Math.max(peak(), limitShown() ? (props.limit?.value ?? 0) : 0), props.binary);
    const x = (t: number) => PAD.left + ((t - t0) / Math.max(t1 - t0, 1)) * (WIDTH - PAD.left - PAD.right);
    const y = (v: number) => PAD.top + (1 - v / axis.top) * (HEIGHT - PAD.top - PAD.bottom);
    const span = t1 - t0;
    return { x, y, ticks: axis.ticks, xTicks: [t0, t0 + span / 2, t1], span };
  });

  const paths = createMemo(() => {
    const { x, y } = scale();
    return segments().map((runsOf) =>
      runsOf
        .filter((run) => run.length > 1)
        .map((run) => run.map((p, i) => `${i ? "L" : "M"}${x(p.t).toFixed(1)},${y(p.v).toFixed(1)}`).join(""))
        .join(""),
    );
  });

  const area = createMemo(() => {
    if (multi()) return "";
    const { x, y } = scale();
    const base = y(0).toFixed(1);
    return (segments()[0] ?? [])
      .filter((run) => run.length > 1)
      .map((run) => {
        const line = run.map((p) => `L${x(p.t).toFixed(1)},${y(p.v).toFixed(1)}`).join("");
        return `M${x(run[0].t).toFixed(1)},${base}${line}L${x(run[run.length - 1].t).toFixed(1)},${base}Z`;
      })
      .join("");
  });

  const lonely = createMemo(() =>
    segments().map((runsOf) => runsOf.filter((run) => run.length === 1).map((run) => run[0])),
  );

  const lastT = () => times()[times().length - 1];
  const at = () => {
    const i = hover();
    return i == null ? lastT() : times()[i];
  };
  const fresh = () => {
    const t = lastT();
    return t != null && Date.now() / 1000 - t <= (props.gap ?? Number.POSITIVE_INFINITY);
  };
  const valueAt = (s: number, t: number | undefined) =>
    t == null ? undefined : samples()[s].find((p) => p.t === t)?.v;
  const total = () => {
    const t = at();
    let sum: number | undefined;
    samples().forEach((_, s) => {
      const v = valueAt(s, t);
      if (v != null) sum = (sum ?? 0) + v;
    });
    return sum;
  };

  function nearest(clientX: number, svg: SVGSVGElement) {
    const rect = svg.getBoundingClientRect();
    const px = ((clientX - rect.left) / rect.width) * WIDTH;
    const { x } = scale();
    let best = 0;
    times().forEach((t, i) => {
      if (Math.abs(x(t) - px) < Math.abs(x(times()[best]) - px)) best = i;
    });
    setHover(times().length ? best : null);
  }

  function onKey(e: KeyboardEvent) {
    const n = times().length;
    if (!n) return;
    const i = hover() ?? n - 1;
    if (e.key === "ArrowLeft") setHover(Math.max(0, i - 1));
    else if (e.key === "ArrowRight") setHover(Math.min(n - 1, i + 1));
    else if (e.key === "Escape") setHover(null);
    else return;
    e.preventDefault();
  }

  const limitText = () => {
    const limit = props.limit;
    return limit ? (limit.text ?? `${limit.label} ${props.format(limit.value)}`) : "";
  };

  return (
    <figure class="card chart-card" style={{ margin: 0 }}>
      <figcaption class="cc-h">
        <span class="cc-t">
          {props.title}
          <Show when={props.limit && !limitShown()}>
            <span class="cc-limit">↑ {limitText()}</span>
          </Show>
        </span>
        <Show when={total() != null}>
          <span class="cc-v">
            <b>{props.format(total() ?? 0)}</b>
            <span>
              {multi() ? "total, " : ""}
              {hover() == null && fresh() ? "now" : stamp(at() ?? 0)}
            </span>
          </span>
        </Show>
      </figcaption>
      <Show when={props.hint}>
        <p class="cc-hint">{props.hint}</p>
      </Show>
      <Show
        when={times().length > 0}
        fallback={
          <Show when={!props.loading} fallback={<span class="bone chart-plot" />}>
            <div class="chart-empty">
              <EmptyState compact icon="activity" title="No data in this range yet" />
            </div>
          </Show>
        }
      >
        <svg
          viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
          role="img"
          aria-label={`${props.title}, latest ${total() != null ? props.format(total() ?? 0) : "none"}. Use left and right arrows to read values.`}
          tabindex="0"
          onPointerMove={(e) => nearest(e.clientX, e.currentTarget)}
          onPointerLeave={() => setHover(null)}
          onKeyDown={onKey}
          onBlur={() => setHover(null)}
        >
          <defs>
            <linearGradient id={gradient} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0" stop-color="currentColor" stop-opacity="0.16" />
              <stop offset="1" stop-color="currentColor" stop-opacity="0" />
            </linearGradient>
          </defs>
          <For each={scale().ticks}>
            {(tick) => (
              <>
                <line class="nc-grid" x1={PAD.left} x2={WIDTH - PAD.right} y1={scale().y(tick)} y2={scale().y(tick)} />
                <text class="nc-tick" x={PAD.left - 8} y={scale().y(tick) + 4} text-anchor="end">
                  {(props.tick ?? props.format)(tick)}
                </text>
              </>
            )}
          </For>
          <For each={scale().xTicks}>
            {(t, i) => (
              <text
                class="nc-tick"
                x={scale().x(t)}
                y={HEIGHT - 4}
                text-anchor={i() === 0 ? "start" : i() === 2 ? "end" : "middle"}
              >
                {niceTime(t, scale().span)}
              </text>
            )}
          </For>
          <Show when={limitShown() && props.limit}>
            {(limit) => (
              <>
                <line
                  class="nc-limit"
                  x1={PAD.left}
                  x2={WIDTH - PAD.right}
                  y1={scale().y(limit().value)}
                  y2={scale().y(limit().value)}
                />
                <text
                  class="nc-tick"
                  x={WIDTH - PAD.right - 2}
                  y={Math.max(PAD.top + 10, scale().y(limit().value) - 5)}
                  text-anchor="end"
                >
                  {limitText()}
                </text>
              </>
            )}
          </Show>
          <Show when={!multi()}>
            <path d={area()} fill={`url(#${gradient})`} />
          </Show>
          <For each={lines()}>
            {(s, i) => (
              <>
                <path
                  class="nc-line"
                  d={paths()[i()]}
                  style={multi() ? { stroke: seriesColor(s.label, i()) } : undefined}
                />
                <For each={lonely()[i()]}>
                  {(p) => (
                    <circle
                      class="nc-lone"
                      cx={scale().x(p.t)}
                      cy={scale().y(p.v)}
                      r={1.75}
                      style={multi() ? { fill: seriesColor(s.label, i()) } : undefined}
                    />
                  )}
                </For>
              </>
            )}
          </For>
          <Show when={hover() != null && at() != null}>
            <line
              class="nc-cross"
              x1={scale().x(at() ?? 0)}
              x2={scale().x(at() ?? 0)}
              y1={PAD.top}
              y2={HEIGHT - PAD.bottom}
            />
          </Show>
          <For each={lines()}>
            {(s, i) => (
              <Show when={valueAt(i(), at()) != null}>
                <circle
                  class="nc-dot"
                  cx={scale().x(at() ?? 0)}
                  cy={scale().y(valueAt(i(), at()) ?? 0)}
                  r={multi() ? 3 : 3.5}
                  style={multi() ? { fill: seriesColor(s.label, i()) } : undefined}
                />
              </Show>
            )}
          </For>
        </svg>
        <Show when={multi()}>
          <ul class="cc-legend">
            <For each={lines()}>
              {(s, i) => (
                <li>
                  <span class="sw-c" style={{ background: seriesColor(s.label, i()) }} />
                  <span class="grow mono">{s.label}</span>
                  <span class="mono muted">
                    {valueAt(i(), at()) != null ? props.format(valueAt(i(), at()) ?? 0) : "none"}
                  </span>
                </li>
              )}
            </For>
          </ul>
        </Show>
      </Show>
    </figure>
  );
}
