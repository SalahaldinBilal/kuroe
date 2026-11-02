import { createResource, createSignal, Index, Match, onCleanup, Show, Switch } from "solid-js";
import { api, type FileEntry } from "../api";
import { formatBytes, formatTime } from "../format";
import { Note } from "./ui/Blocks";
import Button from "./ui/Button/Button";
import EmptyState, { Loading } from "./ui/EmptyState/EmptyState";
import { Icon } from "./ui/Icon";
import { ready } from "./ui/Overlay";
import SegmentedControl from "./ui/SegmentedControl/SegmentedControl";

export const TEXT_LIMIT = 2 << 20;

interface Props {
  container: string;
  path: string;
  entry: FileEntry;
  onBack: () => void;
}

export function FilePreview(props: Props) {
  const [meta] = createResource(
    () => ({ c: props.container, p: props.path }),
    ({ c, p }) => api.fileMeta(c, p),
  );
  const kind = () => ready(meta)?.preview ?? null;
  const name = () => props.entry.name;

  onCleanup(() => {
    if (kind() === "video" || kind() === "audio") api.dropMedia(props.container, props.path).catch(() => {});
  });

  return (
    <>
      <div class="drawer-sub" style={{ gap: "8px" }}>
        <Button icon size="sm" tooltip="Back to the folder" onClick={props.onBack}>
          <Icon name="back" />
        </Button>
        <div class="preview-head grow">
          <b class="mono">{name()}</b>
          <Show when={ready(meta)}>
            {(m) => (
              <span>
                {formatBytes(m().size)} · {(m().mode & 0o777).toString(8).padStart(3, "0")} · {formatTime(m().modified)}
              </span>
            )}
          </Show>
        </div>
        <Button size="sm" download href={api.downloadUrl(props.container, props.path)}>
          <Icon name="download" />
          Download
        </Button>
      </div>
      <div class="preview-body">
        <Show
          when={ready(meta)}
          fallback={
            <Show when={meta.error} fallback={<Loading />}>
              <div class="card-b">
                <Note tone="bad">{String(meta.error?.message ?? meta.error)}</Note>
              </div>
            </Show>
          }
        >
          {(m) => (
            <Switch fallback={<NoPreview size={m().size} container={props.container} path={props.path} />}>
              <Match when={m().kind === "dir"}>
                <EmptyState compact icon="folder" title="This is a folder" />
              </Match>
              <Match when={kind() === "text"}>
                <TextPreview container={props.container} path={props.path} size={m().size} />
              </Match>
              <Match when={kind() === "image" || kind() === "svg"}>
                <ImagePreview src={api.previewUrl(props.container, props.path)} name={name()} />
              </Match>
              <Match when={kind() === "video"}>
                {/* biome-ignore lint/a11y/useMediaCaption: files from a container have no captions */}
                <video
                  class="preview-media"
                  controls
                  preload="metadata"
                  src={api.mediaUrl(props.container, props.path)}
                />
              </Match>
              <Match when={kind() === "audio"}>
                {/* biome-ignore lint/a11y/useMediaCaption: files from a container have no captions */}
                <audio
                  class="preview-audio"
                  controls
                  preload="metadata"
                  src={api.mediaUrl(props.container, props.path)}
                />
              </Match>
              <Match when={kind() === "pdf"}>
                <iframe
                  class="preview-frame"
                  title={name()}
                  sandbox="allow-scripts"
                  src={api.previewUrl(props.container, props.path)}
                />
              </Match>
            </Switch>
          )}
        </Show>
      </div>
    </>
  );
}

function NoPreview(props: { size: number; container: string; path: string }) {
  return (
    <EmptyState
      icon="file"
      title="No preview for this file type"
      description={formatBytes(props.size)}
      actions={
        <Button size="sm" download href={api.downloadUrl(props.container, props.path)}>
          <Icon name="download" />
          Download
        </Button>
      }
    />
  );
}

export function numberedLines(text: string): string[] {
  const lines = text.split("\n");
  if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
  return lines;
}

function TextPreview(props: { container: string; path: string; size: number }) {
  const [text] = createResource(
    () => ({ c: props.container, p: props.path }),
    ({ c, p }) => api.previewText(c, p, TEXT_LIMIT),
  );
  const [wrap, setWrap] = createSignal(false);
  return (
    <Show
      when={text.state === "ready"}
      fallback={
        <Show when={text.error} fallback={<Loading />}>
          <div class="card-b">
            <Note tone="bad">{String(text.error?.message ?? text.error)}</Note>
          </div>
        </Show>
      }
    >
      <div class="preview-tools">
        <Show when={props.size > TEXT_LIMIT}>
          <span class="hint grow">
            Showing the first {formatBytes(TEXT_LIMIT)} of {formatBytes(props.size)}.
          </span>
        </Show>
        <span class="grow" />
        <SegmentedControl
          label="Line wrapping"
          value={wrap() ? "wrap" : "nowrap"}
          onChange={(v) => setWrap(v === "wrap")}
          options={[
            { value: "nowrap", label: "No wrap" },
            { value: "wrap", label: "Wrap" },
          ]}
        />
      </div>
      <div class={wrap() ? "viewer wrap" : "viewer"}>
        <Index each={numberedLines(text() ?? "")}>
          {(line, i) => (
            <div class="vl">
              <span class="ln">{i + 1}</span>
              <span class="lt">{line()}</span>
            </div>
          )}
        </Index>
      </div>
    </Show>
  );
}

function ImagePreview(props: { src: string; name: string }) {
  const [actual, setActual] = createSignal(false);
  return (
    <>
      <div class="preview-tools">
        <span class="grow" />
        <SegmentedControl
          label="Image size"
          value={actual() ? "actual" : "fit"}
          onChange={(v) => setActual(v === "actual")}
          options={[
            { value: "fit", label: "Fit" },
            { value: "actual", label: "Actual size" },
          ]}
        />
      </div>
      <div class={actual() ? "preview-image actual" : "preview-image"}>
        <img src={props.src} alt={props.name} />
      </div>
    </>
  );
}
