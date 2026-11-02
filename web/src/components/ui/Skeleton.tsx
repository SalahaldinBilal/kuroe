import { For, type JSX, Show } from "solid-js";
import { Logo } from "./Logo";
import SettingRow from "./SettingRow/SettingRow";

// Placeholders shaped like the real layout. They reuse the real classes (card, list-i, SettingRow, stat, chart-card)
// so a page keeps its height when the data arrives.

export function Bone(props: {
  w?: string;
  size?: "text" | "sm" | "lg" | "control" | "sm-control" | "pill";
  class?: string;
}) {
  return <span class={`bone ${props.size ?? "text"} ${props.class ?? ""}`} style={{ width: props.w ?? "100%" }} />;
}

/** Wraps placeholders so screen readers hear one "Loading" instead of empty blocks. */
export function Busy(props: { children: JSX.Element }) {
  return (
    <div class="skeleton" aria-busy="true">
      <span class="visually-hidden" role="status">
        Loading
      </span>
      {props.children}
    </div>
  );
}

export function SkeletonCardHead(props: { description?: boolean; action?: boolean }) {
  return (
    <header class="card-h">
      <div class="grow">
        <Bone w="28%" />
        <Show when={props.description}>
          <Bone w="52%" size="sm" />
        </Show>
      </div>
      <Show when={props.action}>
        <Bone w="88px" size="sm-control" />
      </Show>
    </header>
  );
}

/** Shaped like `EmptyState`, for views that usually end in one ("All good", "No tokens"). */
export function SkeletonEmpty(props: { compact?: boolean; action?: boolean; lines?: number }) {
  return (
    <div class="skel-empty" classList={{ compact: props.compact }}>
      <span class="bone empty-icon" />
      <Bone w="120px" />
      <For each={Array.from({ length: props.lines ?? 1 })}>
        {(_, i) => <Bone w={i() === 0 ? "200px" : "140px"} size="sm" class="empty-line" />}
      </For>
      <Show when={props.action}>
        <Bone w="96px" size="sm-control" class="empty-action" />
      </Show>
    </div>
  );
}

/** A `dl.dl` of label and value rows, like the Docker host card. */
export function SkeletonDetails(props: { rows: number }) {
  return (
    <dl class="dl">
      <For each={Array.from({ length: props.rows })}>
        {(_, i) => (
          <>
            <dt>
              <Bone w={`${40 + ((i() * 11) % 30)}%`} />
            </dt>
            <dd>
              <Bone w="45%" class="end" />
            </dd>
          </>
        )}
      </For>
    </dl>
  );
}

function ListRows(props: { rows: number; icon?: boolean; meta?: boolean; actions?: number }) {
  return (
    <For each={Array.from({ length: props.rows })}>
      {(_, i) => (
        <div class="list-i">
          <Show when={props.icon ?? true}>
            <span class="bone avatar" />
          </Show>
          <div class="nm grow">
            <Bone w={`${30 + ((i() * 17) % 25)}%`} />
            <Show when={props.meta ?? true}>
              <Bone w={`${45 + ((i() * 23) % 30)}%`} size="sm" />
            </Show>
          </div>
          <For each={Array.from({ length: props.actions ?? 1 })}>{() => <Bone w="64px" size="sm-control" />}</For>
        </div>
      )}
    </For>
  );
}

/** Rows shaped like `.list-i`, in a card with a header unless `bare`. */
export function SkeletonList(props: {
  rows: number;
  head?: boolean;
  description?: boolean;
  icon?: boolean;
  meta?: boolean;
  actions?: number;
  bare?: boolean;
}) {
  const rows = () => <ListRows rows={props.rows} icon={props.icon} meta={props.meta} actions={props.actions} />;
  return (
    <Show when={!props.bare} fallback={rows()}>
      <section class="card">
        <Show when={props.head ?? true}>
          <SkeletonCardHead description={props.description} />
        </Show>
        {rows()}
      </section>
    </Show>
  );
}

function DescriptionLines(props: { lines: number }) {
  return (
    <For each={Array.from({ length: props.lines })}>
      {(_, i) => <Bone w={i() === props.lines - 1 && props.lines > 1 ? "45%" : "88%"} size="sm" />}
    </For>
  );
}

/**
 * A card of `SettingRow`s, the layout of Settings, Proxy and service settings. `lines` gives each row's description
 * length in lines (0 for none), which is what sets a row's height.
 */
export function SkeletonSettings(props: { rows?: number; lines?: number[]; description?: boolean; head?: boolean }) {
  const lines = () => props.lines ?? Array.from({ length: props.rows ?? 1 }, () => 1);
  return (
    <section class="card">
      <Show when={props.head ?? true}>
        <SkeletonCardHead description={props.description} />
      </Show>
      <For each={lines()}>
        {(count, i) => (
          <SettingRow
            label={<Bone w={`${22 + ((i() * 13) % 18)}%`} />}
            description={count > 0 ? <DescriptionLines lines={count} /> : undefined}
          >
            <Bone size="control" />
          </SettingRow>
        )}
      </For>
    </section>
  );
}

/** The sidebar and an empty page, while kuroe checks the session on a full page load. */
export function SkeletonShell() {
  return (
    <div class="app">
      <aside class="side">
        <div class="brand">
          <Logo />
          kuroe
        </div>
        <div class="nav">
          <For each={Array.from({ length: 4 })}>
            {() => (
              <span class="nav-i">
                <Bone w="70%" />
              </span>
            )}
          </For>
          <div class="nav-label">
            <Bone w="40%" size="sm" />
          </div>
          <For each={Array.from({ length: 7 })}>
            {() => (
              <span class="nav-i">
                <Bone w="60%" />
              </span>
            )}
          </For>
        </div>
      </aside>
      <main class="content" aria-busy="true">
        <span class="visually-hidden" role="status">
          Loading
        </span>
      </main>
    </div>
  );
}

/** Chart cards with the same height as `LineChart`. */
export function SkeletonCharts(props: { count: number; wide?: boolean }) {
  return (
    <div class={props.wide ? "charts wide" : "charts"}>
      <For each={Array.from({ length: props.count })}>
        {() => (
          <figure class="card chart-card" style={{ margin: 0 }}>
            <figcaption class="cc-h">
              <Bone w="30%" size="sm" />
              <Bone w="22%" size="lg" />
            </figcaption>
            <span class="bone chart-plot" />
          </figure>
        )}
      </For>
    </div>
  );
}

/** Stat cards like the dashboard's pool cards. `note` adds the two-line note some of them carry. */
export function SkeletonStats(props: { count: number; note?: boolean }) {
  return (
    <div class="grid3">
      <For each={Array.from({ length: props.count })}>
        {() => (
          <section class="card stat">
            <div class="k">
              <Bone w="40%" />
            </div>
            <div class="v">
              <span class="bone stat-value" />
            </div>
            <span class="bone meter-bone" />
            <div class="pool-legend">
              <Bone w="38%" size="sm" />
            </div>
            <Show when={props.note}>
              <div class="pool-note">
                <DescriptionLines lines={2} />
              </div>
            </Show>
          </section>
        )}
      </For>
    </div>
  );
}

function TableBody(props: { rows: number; columns: number }) {
  return (
    <table class="tbl">
      <thead>
        <tr>
          <For each={Array.from({ length: props.columns })}>
            {() => (
              <th>
                <Bone w="50%" size="sm" />
              </th>
            )}
          </For>
        </tr>
      </thead>
      <tbody>
        <For each={Array.from({ length: props.rows })}>
          {(_, r) => (
            <tr>
              <For each={Array.from({ length: props.columns })}>
                {(_, c) => (
                  <td>
                    <Bone w={c() === 0 ? `${55 + ((r() * 19) % 30)}%` : "60%"} />
                  </td>
                )}
              </For>
            </tr>
          )}
        </For>
      </tbody>
    </table>
  );
}

/** A table with a header row and body rows of `.tbl` height, in a card unless `bare`. */
export function SkeletonTable(props: { rows: number; columns: number; bare?: boolean }) {
  return (
    <Show when={!props.bare} fallback={<TableBody rows={props.rows} columns={props.columns} />}>
      <section class="card">
        <TableBody rows={props.rows} columns={props.columns} />
      </section>
    </Show>
  );
}
