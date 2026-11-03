import { createEffect, createResource, createSignal, For, onCleanup, Show } from "solid-js";
import { stripAnsi } from "../ansi";
import { api, type LogFile } from "../api";
import { formatBytes } from "../format";
import { AnsiText } from "./AnsiText";
import { Card, Pill } from "./ui/Blocks";
import EmptyState from "./ui/EmptyState/EmptyState";
import { Icon } from "./ui/Icon";
import Input from "./ui/Input/Input";
import { ready } from "./ui/Overlay";
import { Busy, SkeletonCardHead, SkeletonList } from "./ui/Skeleton";

interface Props {
  deployment: string;
}

const MAX_CHARS = 2_000_000;

export function LogHistory(props: Props) {
  const [files] = createResource(() => props.deployment, api.logFiles);
  const [selected, setSelected] = createSignal<LogFile | null>(null);
  const [text, setText] = createSignal("");
  const [filter, setFilter] = createSignal("");
  let output: HTMLPreElement | undefined;

  const today = () => new Date().toISOString().slice(0, 10);
  const list = () => ready(files) ?? [];

  createEffect(() => {
    const first = list()[0];
    if (!selected() && first) setSelected(first);
  });

  createEffect(() => {
    const file = selected();
    if (!file) return;
    setText("");
    const socket = new WebSocket(api.logSocket(props.deployment, file.service, file.date));
    socket.addEventListener("message", (e) => {
      const stick = !output || output.scrollTop + output.clientHeight >= output.scrollHeight - 8;
      setText((t) => (t + String(e.data)).slice(-MAX_CHARS));
      if (stick && output) output.scrollTop = output.scrollHeight;
    });
    onCleanup(() => socket.close());
  });

  const shown = () => {
    const needle = filter().trim().toLowerCase();
    if (!needle) return text();
    return text()
      .split("\n")
      .filter((line) => stripAnsi(line).toLowerCase().includes(needle))
      .join("\n");
  };

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
              <pre class="term" ref={output} style={{ height: "520px" }}>
                <AnsiText text={shown()} />
              </pre>
            </Card>
          )}
        </Show>
      </div>
    </Show>
  );
}
