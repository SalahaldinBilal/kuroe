import { createMemo, createSignal, createUniqueId, For, Show } from "solid-js";
import EmptyState from "./ui/EmptyState/EmptyState";

export interface Point {
  t: number;
  v: number | null;
}

interface Props {
  title: string;
  /** One line under the title saying how to read the chart. */
  hint?: string;
  points: Point[];
  format: (v: number) => string;
  max?: number;
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

function niceStep(span: number, target: number): number {
  const raw = span / target;
  const power = 10 ** Math.floor(Math.log10(raw || 1));
  const n = raw / power;
  return (n <= 1 ? 1 : n <= 2 ? 2 : n <= 5 ? 5 : 10) * power;
}

function timeLabel(t: number, span: number): string {
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
  const values = createMemo(() => props.points.filter((p): p is Sample => p.v != null));
  const segments = createMemo(() => runs(props.points, props.gap ?? Number.POSITIVE_INFINITY));

  const scale = createMemo(() => {
    const pts = values();
    const t0 = props.domain?.[0] ?? pts[0]?.t ?? 0;
    const t1 = props.domain?.[1] ?? pts[pts.length - 1]?.t ?? t0 + 1;
    const top = Math.max(props.max ?? 0, props.limit?.value ?? 0, ...pts.map((p) => p.v), 1e-9);
    const step = niceStep(top, 3);
    const yMax = Math.ceil(top / step) * step;
    const x = (t: number) => PAD.left + ((t - t0) / Math.max(t1 - t0, 1)) * (WIDTH - PAD.left - PAD.right);
    const y = (v: number) => PAD.top + (1 - v / yMax) * (HEIGHT - PAD.top - PAD.bottom);
    const ticks = Array.from({ length: Math.round(yMax / step) + 1 }, (_, i) => i * step);
    const span = t1 - t0;
    const xTicks = [t0, t0 + span / 2, t1];
    return { x, y, ticks, xTicks, span };
  });

  const path = createMemo(() => {
    const { x, y } = scale();
    return segments()
      .filter((run) => run.length > 1)
      .map((run) => run.map((p, i) => `${i ? "L" : "M"}${x(p.t).toFixed(1)},${y(p.v).toFixed(1)}`).join(""))
      .join("");
  });

  const area = createMemo(() => {
    const { x, y } = scale();
    const base = y(0).toFixed(1);
    return segments()
      .filter((run) => run.length > 1)
      .map((run) => {
        const line = run.map((p) => `L${x(p.t).toFixed(1)},${y(p.v).toFixed(1)}`).join("");
        return `M${x(run[0].t).toFixed(1)},${base}${line}L${x(run[run.length - 1].t).toFixed(1)},${base}Z`;
      })
      .join("");
  });

  const lonely = createMemo(() =>
    segments()
      .filter((run) => run.length === 1)
      .map((run) => run[0]),
  );

  const last = () => values()[values().length - 1];
  const fresh = () => {
    const p = last();
    const gap = props.gap ?? Number.POSITIVE_INFINITY;
    return !!p && Date.now() / 1000 - p.t <= gap;
  };
  const shown = () => {
    const i = hover();
    return i == null ? last() : values()[i];
  };

  function nearest(clientX: number, svg: SVGSVGElement) {
    const rect = svg.getBoundingClientRect();
    const px = ((clientX - rect.left) / rect.width) * WIDTH;
    const { x } = scale();
    let best = 0;
    values().forEach((p, i) => {
      if (Math.abs(x(p.t) - px) < Math.abs(x(values()[best].t) - px)) best = i;
    });
    setHover(values().length ? best : null);
  }

  function onKey(e: KeyboardEvent) {
    const n = values().length;
    if (!n) return;
    const i = hover() ?? n - 1;
    if (e.key === "ArrowLeft") setHover(Math.max(0, i - 1));
    else if (e.key === "ArrowRight") setHover(Math.min(n - 1, i + 1));
    else if (e.key === "Escape") setHover(null);
    else return;
    e.preventDefault();
  }

  return (
    <figure class="card chart-card" style={{ margin: 0 }}>
      <figcaption class="cc-h">
        <span class="cc-t">{props.title}</span>
        <Show when={shown()}>
          {(p) => (
            <span class="cc-v">
              <b>{props.format(p().v)}</b>
              <span>{hover() == null && fresh() ? "now" : stamp(p().t)}</span>
            </span>
          )}
        </Show>
      </figcaption>
      <Show when={props.hint}>
        <p class="cc-hint">{props.hint}</p>
      </Show>
      <Show
        when={values().length > 0}
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
          aria-label={`${props.title}, latest ${last() ? props.format(last().v) : "none"}. Use left and right arrows to read values.`}
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
                  {props.format(tick)}
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
                {timeLabel(t, scale().span)}
              </text>
            )}
          </For>
          <Show when={props.limit}>
            {(limit) => (
              <>
                <line
                  class="nc-limit"
                  x1={PAD.left}
                  x2={WIDTH - PAD.right}
                  y1={scale().y(limit().value)}
                  y2={scale().y(limit().value)}
                />
                <text class="nc-tick" x={WIDTH - PAD.right - 2} y={scale().y(limit().value) - 5} text-anchor="end">
                  {limit().text ?? `${limit().label} ${props.format(limit().value)}`}
                </text>
              </>
            )}
          </Show>
          <path d={area()} fill={`url(#${gradient})`} />
          <path class="nc-line" d={path()} />
          <For each={lonely()}>
            {(p) => <circle class="nc-lone" cx={scale().x(p.t)} cy={scale().y(p.v)} r={1.75} />}
          </For>
          <Show when={hover() != null && shown()}>
            {(p) => (
              <line
                class="nc-cross"
                x1={scale().x(p().t)}
                x2={scale().x(p().t)}
                y1={PAD.top}
                y2={HEIGHT - PAD.bottom}
              />
            )}
          </Show>
          <Show when={shown()}>
            {(p) => <circle class="nc-dot" cx={scale().x(p().t)} cy={scale().y(p().v)} r={3.5} />}
          </Show>
        </svg>
      </Show>
    </figure>
  );
}
