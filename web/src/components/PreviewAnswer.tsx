import { createMemo, createSignal, For, Index, type JSX, Match, onCleanup, Show, Switch } from "solid-js";
import type { PreviewAnswer as Answer } from "../api";
import { formatBytes } from "../format";
import { highlight } from "../highlight";
import { bodyKind, folderOf, fromBase64, headerValue, hexDump, languageOf, statusTone, withBase } from "../preview";
import { Note, Pill } from "./ui/Blocks";
import Button from "./ui/Button/Button";
import { Icon } from "./ui/Icon";
import SegmentedControl from "./ui/SegmentedControl/SegmentedControl";

const TEXT_LIMIT = 2 << 20;
/** Highlighting more than this makes the page slow, so longer text is shown plain. */
const HIGHLIGHT_LIMIT = 512 << 10;
const HEX_LIMIT = 4096;

interface Props {
  answer: Answer;
  /** Where the request went, for relative links in a rendered page. */
  base: { container: string; port: number; path: string };
}

export function PreviewAnswer(props: Props) {
  const [view, setView] = createSignal<"body" | "headers">("body");
  const bytes = createMemo(() => fromBase64(props.answer.body));
  const type = () => headerValue(props.answer.headers, "content-type");
  const kind = createMemo(() => bodyKind(type(), bytes()));

  return (
    <div class="answer">
      <div class="answer-head">
        <Pill tone={statusTone(props.answer.status)}>
          {props.answer.status}
          <Show when={props.answer.reason}> {props.answer.reason}</Show>
        </Pill>
        <span class="muted mono">{props.answer.time_ms} ms</span>
        <span class="muted mono">{formatBytes(props.answer.size)}</span>
        <Show when={type()}>
          <span class="muted mono answer-type">{type()}</span>
        </Show>
        <span class="grow" />
        <SegmentedControl
          label="Show"
          value={view()}
          onChange={setView}
          options={[
            { value: "body", label: "Body" },
            { value: "headers", label: "Headers", count: props.answer.headers.length },
          ]}
        />
      </div>
      <Show when={props.answer.truncated}>
        <Note tone="warn">Only the first {formatBytes(bytes().length)} came through. The rest was cut off.</Note>
      </Show>
      <Show when={view() === "body"} fallback={<HeaderTable headers={props.answer.headers} />}>
        <Show when={bytes().length > 0} fallback={<p class="answer-empty muted">The answer has no body.</p>}>
          <Switch fallback={<TextBody bytes={bytes()} type={type()} />}>
            <Match when={kind() === "html"}>
              <HtmlBody bytes={bytes()} type={type()} base={props.base} />
            </Match>
            <Match when={kind() === "json"}>
              <JsonBody bytes={bytes()} type={type()} />
            </Match>
            <Match when={kind() === "image"}>
              <BlobView bytes={bytes()} type={type()}>
                {(url) => (
                  <div class="preview-image">
                    <img src={url} alt="The answer's body" />
                  </div>
                )}
              </BlobView>
            </Match>
            <Match when={kind() === "video"}>
              <BlobView bytes={bytes()} type={type()}>
                {(url) => (
                  // biome-ignore lint/a11y/useMediaCaption: an answer from a container has no captions
                  <video class="preview-media" controls src={url} />
                )}
              </BlobView>
            </Match>
            <Match when={kind() === "audio"}>
              <BlobView bytes={bytes()} type={type()}>
                {(url) => (
                  // biome-ignore lint/a11y/useMediaCaption: an answer from a container has no captions
                  <audio class="preview-audio" controls src={url} />
                )}
              </BlobView>
            </Match>
            <Match when={kind() === "pdf"}>
              <BlobView bytes={bytes()} type="application/pdf">
                {(url) => <iframe class="preview-frame" title="PDF from the answer" src={url} />}
              </BlobView>
            </Match>
            <Match when={kind() === "binary"}>
              <BinaryBody bytes={bytes()} type={type()} />
            </Match>
          </Switch>
        </Show>
      </Show>
    </div>
  );
}

function HeaderTable(props: { headers: [string, string][] }) {
  return (
    <table class="tbl answer-headers">
      <tbody>
        <For each={props.headers}>
          {([name, value]) => (
            <tr>
              <td class="mono">{name}</td>
              <td class="mono">{value}</td>
            </tr>
          )}
        </For>
      </tbody>
    </table>
  );
}

/** A blob URL for the bytes, freed when the view goes. */
function BlobView(props: { bytes: Uint8Array<ArrayBuffer>; type?: string; children: (url: string) => JSX.Element }) {
  const url = createMemo(() => {
    const made = URL.createObjectURL(new Blob([props.bytes], { type: props.type ?? "" }));
    onCleanup(() => URL.revokeObjectURL(made));
    return made;
  });
  return <>{props.children(url())}</>;
}

function decode(bytes: Uint8Array, limit: number): string {
  return new TextDecoder().decode(bytes.subarray(0, limit));
}

function Lines(props: { text: string; type?: string; wrap: boolean }) {
  const lines = createMemo(() =>
    props.text.length > HIGHLIGHT_LIMIT
      ? props.text.split("\n").map((line) => [{ kind: "plain" as const, text: line }])
      : highlight(props.text, languageOf(props.type)),
  );
  return (
    <div class={props.wrap ? "viewer wrap" : "viewer"}>
      <Index each={lines()}>
        {(line, i) => (
          <div class="vl">
            <span class="ln">{i + 1}</span>
            <span class="lt">
              <For each={line()}>
                {(token) =>
                  token.kind === "plain" ? token.text : <span class={`hl-${token.kind}`}>{token.text}</span>
                }
              </For>
            </span>
          </div>
        )}
      </Index>
    </div>
  );
}

function WrapToggle(props: { wrap: boolean; onChange: (wrap: boolean) => void }) {
  return (
    <SegmentedControl
      label="Line wrapping"
      value={props.wrap ? "wrap" : "nowrap"}
      onChange={(v) => props.onChange(v === "wrap")}
      options={[
        { value: "nowrap", label: "No wrap" },
        { value: "wrap", label: "Wrap" },
      ]}
    />
  );
}

function TextBody(props: { bytes: Uint8Array; type?: string }) {
  const [wrap, setWrap] = createSignal(false);
  return (
    <>
      <div class="preview-tools">
        <Show when={props.bytes.length > TEXT_LIMIT}>
          <span class="hint">Showing the first {formatBytes(TEXT_LIMIT)}.</span>
        </Show>
        <span class="grow" />
        <WrapToggle wrap={wrap()} onChange={setWrap} />
      </div>
      <Lines text={decode(props.bytes, TEXT_LIMIT)} type={props.type} wrap={wrap()} />
    </>
  );
}

function HtmlBody(props: { bytes: Uint8Array; type?: string; base: Props["base"] }) {
  const [source, setSource] = createSignal(false);
  const html = () => decode(props.bytes, TEXT_LIMIT);
  const href = () => `/preview/${props.base.container}/${props.base.port}${folderOf(props.base.path)}`;
  return (
    <>
      <div class="preview-tools">
        <span class="hint grow">Rendered without scripts.</span>
        <SegmentedControl
          label="HTML view"
          value={source() ? "source" : "page"}
          onChange={(v) => setSource(v === "source")}
          options={[
            { value: "page", label: "Page" },
            { value: "source", label: "Source" },
          ]}
        />
      </div>
      <Show
        when={source()}
        fallback={
          // No allow-scripts: the page can't run code, so letting it load its own styles and images from kuroe is safe.
          <iframe
            class="preview-frame"
            title="Rendered answer"
            sandbox="allow-same-origin"
            srcdoc={withBase(html(), href())}
          />
        }
      >
        <Lines text={html()} type="text/html" wrap={true} />
      </Show>
    </>
  );
}

function JsonBody(props: { bytes: Uint8Array; type?: string }) {
  const [raw, setRaw] = createSignal(false);
  const text = () => decode(props.bytes, TEXT_LIMIT);
  const parsed = createMemo((): { value: unknown } | null => {
    try {
      return { value: JSON.parse(text()) };
    } catch {
      return null;
    }
  });
  return (
    <>
      <div class="preview-tools">
        <Show when={!parsed()}>
          <span class="hint">This isn't valid JSON, so it's shown as text.</span>
        </Show>
        <span class="grow" />
        <Show when={parsed()}>
          <SegmentedControl
            label="JSON view"
            value={raw() ? "raw" : "tree"}
            onChange={(v) => setRaw(v === "raw")}
            options={[
              { value: "tree", label: "Tree" },
              { value: "raw", label: "Raw" },
            ]}
          />
        </Show>
      </div>
      <Show when={parsed() && !raw()} fallback={<Lines text={pretty(text())} type="application/json" wrap={true} />}>
        <div class="json-tree">
          <JsonNode value={parsed()?.value} depth={0} />
        </div>
      </Show>
    </>
  );
}

function pretty(text: string): string {
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch {
    return text;
  }
}

/** Objects and arrays fold. The first two levels start open. */
function JsonNode(props: { value: unknown; name?: string; depth: number }) {
  const label = () => (props.name === undefined ? null : <span class="hl-attr">{props.name}: </span>);
  const entries = (): [string, unknown][] => {
    if (props.value === null || typeof props.value !== "object") return [];
    return Array.isArray(props.value)
      ? props.value.map((v, i) => [String(i), v])
      : Object.entries(props.value as Record<string, unknown>);
  };
  const container = () => props.value !== null && typeof props.value === "object" && entries().length > 0;
  return (
    <Show
      when={container()}
      fallback={
        <div class="jn">
          {label()}
          <JsonLeaf value={props.value} />
        </div>
      }
    >
      <details class="jn" open={props.depth < 2}>
        <summary>
          {label()}
          <span class="muted">{Array.isArray(props.value) ? `[${entries().length}]` : `{${entries().length}}`}</span>
        </summary>
        <div class="jc">
          <For each={entries()}>
            {([name, value]) => <JsonNode name={name} value={value} depth={props.depth + 1} />}
          </For>
        </div>
      </details>
    </Show>
  );
}

function JsonLeaf(props: { value: unknown }) {
  const v = () => props.value;
  if (typeof v() === "string") return <span class="hl-string">{JSON.stringify(v())}</span>;
  if (typeof v() === "number") return <span class="hl-number">{String(v())}</span>;
  if (Array.isArray(v())) return <span class="muted">[]</span>;
  if (v() !== null && typeof v() === "object") return <span class="muted">{"{}"}</span>;
  return <span class="hl-keyword">{String(v())}</span>;
}

function BinaryBody(props: { bytes: Uint8Array<ArrayBuffer>; type?: string }) {
  return (
    <>
      <div class="preview-tools">
        <span class="hint grow">
          No preview for this type. The first {formatBytes(Math.min(props.bytes.length, HEX_LIMIT))} as hex.
        </span>
        <BlobView bytes={props.bytes} type={props.type}>
          {(url) => (
            <Button size="sm" download href={url}>
              <Icon name="download" />
              Download
            </Button>
          )}
        </BlobView>
      </div>
      <pre class="viewer hex">{hexDump(props.bytes, HEX_LIMIT).join("\n")}</pre>
    </>
  );
}
