import { createEffect, createMemo, createResource, createSignal, For, on, onCleanup, Show } from "solid-js";
import { api, type LogFile } from "../api";
import { formatBytes } from "../format";
import { clockTime, type LogEntry, parseLogLine, splitLines, stampText } from "../logLines";
import { AnsiSegments } from "./AnsiText";
import { Card, Pill } from "./ui/Blocks";
import EmptyState from "./ui/EmptyState/EmptyState";
import { Icon } from "./ui/Icon";
import Input from "./ui/Input/Input";
import { ready } from "./ui/Overlay";
import { Busy, SkeletonCardHead, SkeletonList } from "./ui/Skeleton";
import Switch from "./ui/Switch/Switch";
import Tip from "./ui/Tooltip/Tip";

interface Props {
  deployment: string;
}

const MAX_LINES = 20_000;
const COLUMNS_KEY = "kuroe.logs.columns";

/** A column left undefined uses its default. */
interface Columns {
  time?: boolean;
  stream?: boolean;
  container?: boolean;
}

function loadColumns(): Columns {
  try {
    return JSON.parse(localStorage.getItem(COLUMNS_KEY) ?? "{}") as Columns;
  } catch {
    return {};
  }
}

function saveColumns(columns: Columns) {
  try {
    localStorage.setItem(COLUMNS_KEY, JSON.stringify(columns));
  } catch {}
}

export function LogHistory(props: Props) {
  const deployment = createMemo(() => props.deployment);
  const [files] = createResource(deployment, api.logFiles);
  const [selected, setSelected] = createSignal<LogFile | null>(null);
  const [entries, setEntries] = createSignal<LogEntry[]>([]);
  const [filter, setFilter] = createSignal("");
  let output: HTMLDivElement | undefined;

  const today = () => new Date().toISOString().slice(0, 10);
  const list = () => ready(files) ?? [];

  createEffect(on(deployment, () => setSelected(null), { defer: true }));

  createEffect(() => {
    if (files.loading) return;
    const first = list()[0];
    if (!selected() && first) setSelected(first);
  });

  function append(lines: string[]) {
    if (!lines.length) return;
    const stick = !output || output.scrollTop + output.clientHeight >= output.scrollHeight - 8;
    setEntries((prev) => {
      const next = prev.concat(lines.map(parseLogLine));
      return next.length > MAX_LINES ? next.slice(-MAX_LINES) : next;
    });
    if (stick && output) output.scrollTop = output.scrollHeight;
  }

  createEffect(
    on(selected, (file) => {
      setEntries([]);
      if (!file) return;
      let pending = "";
      const socket = new WebSocket(api.logSocket(deployment(), file.service, file.date));
      socket.addEventListener("message", (e) => {
        const { lines, rest } = splitLines(pending + String(e.data));
        pending = rest;
        append(lines);
      });
      socket.addEventListener("close", () => {
        if (pending) append([pending]);
        pending = "";
      });
      onCleanup(() => socket.close());
    }),
  );

  const shown = createMemo(() => {
    const needle = filter().trim().toLowerCase();
    return needle ? entries().filter((e) => e.search.includes(needle)) : entries();
  });

  const replicas = createMemo(() => {
    const first = entries().find((e) => e.container)?.container;
    return entries().some((e) => e.container && e.container !== first);
  });

  const [columns, setColumns] = createSignal<Columns>(loadColumns());
  const showTime = () => columns().time ?? true;
  const showStream = () => columns().stream ?? false;
  const showContainer = () => columns().container ?? replicas();
  const showGutter = () => showTime() || showStream() || showContainer();

  function toggle(column: keyof Columns, on: boolean) {
    const next = { ...columns(), [column]: on };
    setColumns(next);
    saveColumns(next);
  }

  return (
    <Show
      when={list().length > 0}
      fallback={
        <Show
          when={files.latest || files.error}
          fallback={
            <Busy>
              <div class="logs-split">
                <SkeletonList rows={2} description icon={false} actions={0} />
                <section class="card">
                  <SkeletonCardHead description action />
                  <span class="bone log-bone" />
                </section>
              </div>
            </Busy>
          }
        >
          <Card>
            <EmptyState
              icon="logs"
              title="No saved logs yet"
              description="Logs are saved per day as soon as a container writes output."
            />
          </Card>
        </Show>
      }
    >
      <div class="logs-split">
        <Card title="Saved logs" description="One file per service and day">
          <For each={list()}>
            {(f) => (
              <button type="button" class={selected() === f ? "list-i on" : "list-i"} onClick={() => setSelected(f)}>
                <div class="nm grow">
                  <b>
                    {f.date}
                    <Show when={f.date === today()}>
                      <Pill>today</Pill>
                    </Show>
                  </b>
                  <span>
                    {f.service} · {formatBytes(f.bytes)}
                  </span>
                </div>
              </button>
            )}
          </For>
        </Card>
        <Show when={selected()}>
          {(file) => (
            <Card
              title={`${file().date} · ${file().service}`}
              description={
                file().date === today()
                  ? "Shows the last 256 KiB, then follows new lines."
                  : "Shows the last 256 KiB of this day."
              }
              actions={
                <Input
                  size="sm"
                  type="search"
                  clearable
                  before={<Icon name="search" />}
                  label="Filter lines"
                  placeholder="Filter lines"
                  value={filter()}
                  onValue={setFilter}
                />
              }
            >
              <div class="log-tools">
                <span>Show</span>
                <Switch label="Time" checked={showTime()} onChange={(on) => toggle("time", on)} />
                <Switch label="Stream" checked={showStream()} onChange={(on) => toggle("stream", on)} />
                <Switch label="Container" checked={showContainer()} onChange={(on) => toggle("container", on)} />
              </div>
              <div class="term log-view" ref={output} style={{ height: "520px" }}>
                <For each={shown()}>
                  {(entry) => (
                    <div class="log-line">
                      <Show when={showGutter()}>
                        <span class="log-gutter">
                          <Show when={showTime()}>
                            <Show when={entry.stamp} fallback={<span class="log-time" />}>
                              {(stamp) => (
                                <Tip content={stampText(entry)}>
                                  <span class="log-time">{clockTime(stamp())}</span>
                                </Tip>
                              )}
                            </Show>
                          </Show>
                          <Show when={showStream()}>
                            <span class="log-stream">{entry.stream}</span>
                          </Show>
                          <Show when={showContainer()}>
                            <span class="log-container">{entry.container}</span>
                          </Show>
                        </span>
                      </Show>
                      <span class="log-text">
                        <AnsiSegments line={entry.segments} />
                      </span>
                    </div>
                  )}
                </For>
              </div>
            </Card>
          )}
        </Show>
      </div>
    </Show>
  );
}
