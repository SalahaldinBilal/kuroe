import { createEffect, createSignal, For, on, Show } from "solid-js";
import { api, type FileEntry, type FolderSizes, type Listing } from "../api";
import { formatBytes, formatTime } from "../format";
import useToastState, { errorMessage } from "../states/toastState";
import { FilePreview } from "./FilePreview";
import { Note } from "./ui/Blocks";
import Button from "./ui/Button/Button";
import EmptyState, { Loading } from "./ui/EmptyState/EmptyState";
import { Icon, type IconName } from "./ui/Icon";
import Input from "./ui/Input/Input";
import Tip from "./ui/Tooltip/Tip";

/** Listings kept for the drawer's lifetime, keyed by container and path. */
export type FileCache = Map<string, Listing>;

interface Props {
  container: string;
  cache: FileCache;
}

export function join(dir: string, name: string): string {
  return dir.endsWith("/") ? `${dir}${name}` : `${dir}/${name}`;
}

export function parent(dir: string): string {
  const trimmed = dir.replace(/\/+$/, "");
  const cut = trimmed.lastIndexOf("/");
  return cut <= 0 ? "/" : trimmed.slice(0, cut);
}

const isFolder = (entry: FileEntry) => entry.kind === "dir" || !!entry.link_dir;

export function Files(props: Props) {
  const { pushToast, pushError } = useToastState;
  const [dir, setDir] = createSignal("/");
  const [path, setPath] = createSignal("/");
  const [listing, setListing] = createSignal<Listing | null>(null);
  const [error, setError] = createSignal("");
  const [refreshing, setRefreshing] = createSignal(false);
  const [sizes, setSizes] = createSignal<FolderSizes | null>(null);
  const [opened, setOpened] = createSignal<FileEntry | null>(null);
  let upload!: HTMLInputElement;
  let request = 0;

  const key = (d: string) => `${props.container}:${d}`;

  async function load(d: string) {
    const mine = ++request;
    setRefreshing(true);
    try {
      const fresh = await api.files(props.container, d);
      props.cache.set(key(d), fresh);
      if (mine !== request) return;
      setListing(fresh);
      setError("");
      loadSizes(d, fresh, mine);
    } catch (err) {
      if (mine === request) setError(errorMessage(err));
    } finally {
      if (mine === request) setRefreshing(false);
    }
  }

  async function loadSizes(d: string, list: Listing, mine: number) {
    setSizes(null);
    if (list.note || !list.entries.some((e) => e.kind === "dir")) return setSizes({ sizes: {}, complete: true });
    try {
      const result = await api.folderSizes(props.container, d);
      if (mine === request) setSizes(result);
    } catch {
      if (mine === request) setSizes({ sizes: {}, complete: false });
    }
  }

  createEffect(
    on(dir, (d) => {
      setPath(d);
      setOpened(null);
      const cached = props.cache.get(key(d));
      setListing(cached ?? null);
      setError("");
      load(d);
    }),
  );

  async function onUpload() {
    const file = upload.files?.[0];
    if (!file) return;
    try {
      await api.uploadFile(props.container, dir(), file);
      pushToast(`Uploaded ${file.name}`);
      upload.value = "";
      load(dir());
    } catch (err) {
      pushError(err);
    }
  }

  function open(entry: FileEntry) {
    if (isFolder(entry)) setDir(join(dir(), entry.name));
    else if (entry.kind === "file" || entry.kind === "link") setOpened(entry);
  }

  const icon = (entry: FileEntry): IconName => (isFolder(entry) ? "folder" : entry.kind === "link" ? "link" : "file");

  function folderSize(entry: FileEntry) {
    const s = sizes();
    if (!s) return <span class="faint">…</span>;
    const bytes = s.sizes[entry.name];
    if (bytes !== undefined) return formatBytes(bytes);
    if (listing()?.note) return "";
    return (
      <Tip content={s.complete ? "Couldn't measure this folder" : "Didn't finish in time"}>
        <span class="faint">-</span>
      </Tip>
    );
  }

  return (
    <Show
      when={opened()}
      fallback={
        <>
          <form
            class="drawer-sub"
            style={{ gap: "6px" }}
            onSubmit={(e) => {
              e.preventDefault();
              const next = path().trim() || "/";
              if (next === dir()) load(next);
              else setDir(next);
            }}
          >
            <Button
              icon
              size="sm"
              tooltip="Up one folder"
              disabled={dir() === "/"}
              onClick={() => setDir(parent(dir()))}
            >
              <Icon name="up" />
            </Button>
            <Input mono size="sm" class="grow" label="Path" value={path()} onValue={setPath} />
            <Button type="submit" size="sm">
              Open
            </Button>
            <Button icon size="sm" tooltip="Refresh" disabled={refreshing()} onClick={() => load(dir())}>
              <Icon name="rotate" />
            </Button>
            <Button size="sm" onClick={() => upload.click()}>
              <Icon name="upload" />
              Upload here
            </Button>
            <input ref={upload} class="hidden" type="file" onChange={onUpload} />
          </form>
          <Show when={listing()?.note}>
            {(note) => (
              <div class="card-b">
                <Note>{note()}</Note>
              </div>
            )}
          </Show>
          <Show when={error()}>
            <div class="card-b">
              <Note tone="bad">{error()}</Note>
            </div>
          </Show>
          <Show when={listing()} fallback={<Show when={!error()}>{<Loading />}</Show>}>
            {(list) => (
              <Show
                when={list().entries.length > 0}
                fallback={<EmptyState compact icon="folder" title="Empty folder" />}
              >
                <table class="tbl">
                  <thead>
                    <tr>
                      <th>Name</th>
                      <th class="num">Size</th>
                      <th>Mode</th>
                      <th class="hide-sm">Modified</th>
                      <th />
                    </tr>
                  </thead>
                  <tbody>
                    <For each={list().entries}>
                      {(entry) => (
                        <tr class={entry.kind === "other" ? "" : "click"} onClick={() => open(entry)}>
                          <td>
                            <span
                              class="hrow"
                              style={isFolder(entry) ? { color: "var(--ink)", "font-weight": "500" } : undefined}
                            >
                              <Icon name={icon(entry)} />
                              <span class="mono">
                                {entry.name}
                                {isFolder(entry) ? "/" : ""}
                              </span>
                              <Show when={entry.link}>
                                <span class="faint mono">→ {entry.link}</span>
                              </Show>
                            </span>
                          </td>
                          <td class="num muted">
                            {entry.kind === "dir"
                              ? folderSize(entry)
                              : entry.kind === "file"
                                ? formatBytes(entry.size)
                                : ""}
                          </td>
                          <td class="mono muted">{(entry.mode & 0o777).toString(8).padStart(3, "0")}</td>
                          <td class="hide-sm mono muted">{formatTime(entry.modified)}</td>
                          <td class="acts">
                            <Button
                              variant="ghost"
                              icon
                              size="sm"
                              tooltip="Download"
                              download
                              href={api.downloadUrl(props.container, join(dir(), entry.name))}
                              onClick={(e) => e.stopPropagation()}
                            >
                              <Icon name="download" />
                            </Button>
                          </td>
                        </tr>
                      )}
                    </For>
                  </tbody>
                </table>
              </Show>
            )}
          </Show>
        </>
      }
    >
      {(entry) => (
        <FilePreview
          container={props.container}
          path={join(dir(), entry().name)}
          entry={entry()}
          onBack={() => setOpened(null)}
        />
      )}
    </Show>
  );
}
