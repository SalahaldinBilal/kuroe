import { Index, Show } from "solid-js";
import type { ErrorPage } from "../api";
import { emptyPath, type PathDraft, pageLabel, type RoutingDraft, readErrorPage, retries } from "../routing";
import useToastState from "../states/toastState";
import Button from "./ui/Button/Button";
import Field, { FieldRow } from "./ui/Field/Field";
import { Icon } from "./ui/Icon";
import Input, { Unit } from "./ui/Input/Input";
import SegmentedControl from "./ui/SegmentedControl/SegmentedControl";
import Select, { items } from "./ui/Select/Select";
import Switch from "./ui/Switch/Switch";

interface Props {
  draft: RoutingDraft;
  onChange: (draft: RoutingDraft) => void;
  services: string[];
  site: boolean;
  /** Paths, redirects, compression and error pages need managed Traefik. */
  managed: boolean;
}

const KINDS = [
  { value: "service", label: "A service" },
  { value: "redirect", label: "Another address" },
] as const;

const MATCHES = [
  { value: "prefix", label: "Starts with" },
  { value: "exact", label: "Exactly" },
];

const PAGE_TYPES = ".html,.htm,.json,text/html,application/json";

function pickFile(onFile: (file: File) => void) {
  const input = document.createElement("input");
  input.type = "file";
  input.accept = PAGE_TYPES;
  input.onchange = () => {
    const file = input.files?.[0];
    if (file) onFile(file);
  };
  input.click();
}

function usePagePicker(onPage: (page: ErrorPage) => void) {
  const { pushError } = useToastState;
  return () =>
    pickFile(async (file) => {
      try {
        onPage(await readErrorPage(file));
      } catch (err) {
        pushError(err);
      }
    });
}

function ErrorPageField(props: { page: ErrorPage | null; onChange: (page: ErrorPage | null) => void }) {
  const pick = usePagePicker((page) => props.onChange(page));
  return (
    <div class="hrow">
      <Show
        when={props.page}
        fallback={
          <Button size="sm" onClick={pick}>
            <Icon name="upload" />
            Upload page
          </Button>
        }
      >
        {(page) => (
          <>
            <span class="chip">
              <Icon name="file" />
              {pageLabel(page())}
            </span>
            <Button size="sm" onClick={pick}>
              Replace
            </Button>
            <Button variant="ghost" icon size="sm" tooltip="Remove the error page" onClick={() => props.onChange(null)}>
              <Icon name="x" />
            </Button>
          </>
        )}
      </Show>
    </div>
  );
}

function PathRows(props: { rows: PathDraft[]; services: string[]; onChange: (rows: PathDraft[]) => void }) {
  function patch(i: number, change: Partial<PathDraft>) {
    props.onChange(props.rows.map((r, j) => (j === i ? { ...r, ...change } : r)));
  }
  return (
    <div class="kv">
      <Index each={props.rows}>
        {(row, i) => {
          const pick = usePagePicker((page) => patch(i, { page }));
          return (
            <div class="kvrow">
              <Input mono label="Path" placeholder="/api/" value={row().path} onValue={(path) => patch(i, { path })} />
              <Select
                class="match"
                label="Match"
                items={MATCHES}
                value={row().exact ? "exact" : "prefix"}
                onChange={(match) => patch(i, { exact: match === "exact" })}
              />
              <Select
                mono
                label="Service"
                items={items(
                  props.services.includes(row().service) ? props.services : [row().service, ...props.services],
                )}
                value={row().service}
                onChange={(service) => patch(i, { service })}
              />
              <Input
                mono
                class="port"
                inputMode="numeric"
                label="Port"
                placeholder="port"
                value={row().port}
                onValue={(port) => patch(i, { port })}
              />
              <Button
                variant="ghost"
                icon
                size="sm"
                active={!!row().page}
                tooltip={
                  row().page
                    ? `Remove this path's error page (${pageLabel(row().page as ErrorPage)})`
                    : "Add an error page for this path"
                }
                onClick={() => (row().page ? patch(i, { page: null }) : pick())}
              >
                <Icon name="file" />
              </Button>
              <Button
                variant="ghost"
                icon
                size="sm"
                tooltip="Remove"
                onClick={() => props.onChange(props.rows.filter((_, j) => j !== i))}
              >
                <Icon name="x" />
              </Button>
            </div>
          );
        }}
      </Index>
      <div class="kv-actions">
        <Button size="sm" onClick={() => props.onChange([...props.rows, emptyPath(props.services[0] ?? "")])}>
          <Icon name="plus" />
          Add path
        </Button>
      </div>
    </div>
  );
}

/** Where a domain sends traffic: a service with optional paths, compression and error pages, or a redirect. */
export function DomainRouting(props: Props) {
  const set = (change: Partial<RoutingDraft>) => props.onChange({ ...props.draft, ...change });
  const toService = () => props.draft.kind === "service";

  return (
    <>
      <Show when={props.managed}>
        <Field label="Sends traffic to">
          <SegmentedControl
            fill
            label="Sends traffic to"
            options={KINDS}
            value={props.draft.kind}
            onChange={(kind) => set({ kind })}
          />
        </Field>
      </Show>
      <Show when={toService() && !props.site}>
        <FieldRow>
          <Field label="Service">
            <Select
              mono
              label="Service"
              items={items(props.services)}
              value={props.draft.service}
              onChange={(service) => set({ service })}
            />
          </Field>
          <Field label="Port">
            <Input
              mono
              inputMode="numeric"
              label="Port"
              placeholder="80"
              value={props.draft.port}
              onValue={(port) => set({ port })}
            />
          </Field>
        </FieldRow>
      </Show>
      <Show when={!toService()}>
        <Field label="Redirect to" hint="Visitors land on this address with the same path. The redirect is permanent.">
          <Input
            mono
            label="Redirect to"
            placeholder="https://example.com"
            value={props.draft.redirect}
            onValue={(redirect) => set({ redirect })}
          />
        </Field>
      </Show>
      <Show when={props.managed && toService() && !props.site}>
        <Field label="Paths" hint="Send these paths to another service. A longer path wins over a shorter one.">
          <PathRows rows={props.draft.paths} services={props.services} onChange={(paths) => set({ paths })} />
        </Field>
        <Field
          label="Error page"
          hint="Shown with status 503 when the service doesn't answer. HTML, or JSON for APIs. A path can have its own."
        >
          <ErrorPageField page={props.draft.page} onChange={(page) => set({ page })} />
        </Field>
        <Field
          label="Retries"
          hint="Holds requests while the service restarts. Off by default where there's an error page, so the page shows at once."
        >
          <Switch
            label="Retry while the service restarts"
            checked={retries(props.draft, !!props.draft.page)}
            onChange={(retry) => set({ retry })}
          />
        </Field>
        <FieldRow>
          <Field label="Timeout" hint="To connect and to start answering. Then 504.">
            <Input
              mono
              inputMode="numeric"
              label="Timeout"
              placeholder="none"
              value={props.draft.timeout}
              onValue={(timeout) => set({ timeout })}
              after={<Unit>s</Unit>}
            />
          </Field>
          <Field
            label="Body limit"
            hint="Bigger requests get 413. Answers are held whole, so leave it empty for streaming."
          >
            <Input
              mono
              inputMode="numeric"
              label="Body limit"
              placeholder="none"
              value={props.draft.maxBody}
              onValue={(maxBody) => set({ maxBody })}
              after={<Unit>MB</Unit>}
            />
          </Field>
        </FieldRow>
        <Field
          label="Host header"
          hint="For apps that refuse unknown hostnames. The domain's own service sees its container address. Paths keep the domain."
        >
          <Switch
            label="Send the service's address as Host"
            checked={props.draft.rewriteHost}
            onChange={(rewriteHost) => set({ rewriteHost })}
          />
        </Field>
        <Switch label="Compress responses" checked={props.draft.compress} onChange={(compress) => set({ compress })} />
        <Show when={props.draft.compress}>
          <FieldRow>
            <Field label="Content types" hint="Comma separated. Empty compresses most types.">
              <Input
                mono
                label="Content types"
                placeholder="text/html, text/css"
                value={props.draft.compressTypes}
                onValue={(compressTypes) => set({ compressTypes })}
              />
            </Field>
            <Field label="Minimum size">
              <Input
                mono
                inputMode="numeric"
                label="Minimum size"
                placeholder="1024"
                value={props.draft.compressMin}
                onValue={(compressMin) => set({ compressMin })}
                after={<Unit>bytes</Unit>}
              />
            </Field>
          </FieldRow>
          <Switch
            label="gzip only, no Brotli or Zstandard"
            checked={props.draft.gzipOnly}
            onChange={(gzipOnly) => set({ gzipOnly })}
          />
        </Show>
      </Show>
    </>
  );
}
