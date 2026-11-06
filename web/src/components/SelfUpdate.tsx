import { createResource, createSignal, Match, onCleanup, Show, Switch } from "solid-js";
import { api, type LatestRelease, type VersionInfo } from "../api";
import { errorMessage } from "../states/toastState";
import { Card, ExtLink, Note } from "./ui/Blocks";
import Button from "./ui/Button/Button";
import Field from "./ui/Field/Field";
import Input from "./ui/Input/Input";
import { ready } from "./ui/Overlay";
import { Bone, Busy, SkeletonCardHead } from "./ui/Skeleton";
import Spinner from "./ui/Spinner/Spinner";

const POLL_MS = 1000;
const GIVE_UP_MS = 120_000;

type UpdateBody = { image?: string; binary_url?: string; sha256?: string };

function label(v: VersionInfo): string {
  return v.build ? `${v.version} (${v.build})` : v.version;
}

function canUse(release: LatestRelease, v: VersionInfo): boolean {
  return v.install.mode === "container" || (v.install.mode === "binary" && release.binary_url !== null);
}

export function SelfUpdate() {
  const [current, { refetch }] = createResource(api.version);
  const [status, { refetch: refetchStatus }] = createResource(api.updateStatus);
  const [latest, { refetch: refetchLatest }] = createResource(api.latestRelease);
  const [phase, setPhase] = createSignal<"idle" | "updating" | "done" | "failed">("idle");
  const [message, setMessage] = createSignal("");
  const [target, setTarget] = createSignal("");
  const [started, setStarted] = createSignal(0);
  const [now, setNow] = createSignal(Date.now());
  const [image, setImage] = createSignal<string | null>(null);
  const [binaryUrl, setBinaryUrl] = createSignal("");
  const [sha256, setSha256] = createSignal("");
  let timer: number | undefined;
  onCleanup(() => clearInterval(timer));

  const elapsed = () => Math.max(0, Math.round((now() - started()) / 1000));

  function finish(result: "done" | "failed", text: string) {
    clearInterval(timer);
    setPhase(result);
    setMessage(text);
    refetch();
    refetchStatus();
    refetchLatest();
  }

  function watch(before: string) {
    let wentAway = false;
    timer = window.setInterval(async () => {
      setNow(Date.now());
      try {
        const v = await api.version();
        if (label(v) !== before) {
          finish("done", `Now running ${label(v)}. Back after ${elapsed()} s.`);
          return;
        }
        if (wentAway) {
          const s = await api.updateStatus();
          if (s.state === "exited") {
            finish(
              s.exit_code === 0 ? "done" : "failed",
              s.exit_code === 0 ? "Update finished." : `The update failed and ${before} is running again.`,
            );
            return;
          }
        }
      } catch {
        wentAway = true;
      }
      if (Date.now() - started() > GIVE_UP_MS)
        finish("failed", "No answer for two minutes. Check the updater log below.");
    }, POLL_MS);
  }

  async function start(body: UpdateBody, v: VersionInfo, to: string) {
    clearInterval(timer);
    setTarget(to);
    setStarted(Date.now());
    setNow(Date.now());
    setMessage("");
    setPhase("updating");
    try {
      await api.startUpdate(body);
      watch(label(v));
    } catch (err) {
      setPhase("failed");
      setMessage(errorMessage(err));
    }
  }

  function updateToRelease(release: LatestRelease, v: VersionInfo) {
    const body =
      v.install.mode === "container" ? { image: release.image } : { binary_url: release.binary_url ?? undefined };
    start(body, v, release.version);
  }

  function onSubmit(event: SubmitEvent, v: VersionInfo) {
    event.preventDefault();
    const optional = (value: string) => value.trim() || undefined;
    if (v.install.mode === "container") {
      const chosen = (image() ?? v.install.image).trim();
      start({ image: chosen }, v, chosen);
    } else {
      start({ binary_url: optional(binaryUrl()), sha256: optional(sha256()) }, v, "the new binary");
    }
  }

  return (
    <Show
      when={ready(current)}
      fallback={
        <Show when={!current.error}>
          <Busy>
            <section class="card">
              <SkeletonCardHead description />
              <div class="card-b hrow">
                <Bone size="control" class="grow" />
                <Bone w="82px" size="control" />
              </div>
            </section>
          </Busy>
        </Show>
      }
    >
      {(v) => (
        <Card
          title="Update kuroe"
          description={
            <>
              Running <span class="mono">{label(v())}</span>
              {v().install.mode === "container" ? " from a container" : ""}. Deployments keep running during the update.
            </>
          }
        >
          <div class="card-b stack" style={{ gap: "12px" }}>
            <Show
              when={phase() !== "updating"}
              fallback={
                <div class="note update-progress">
                  <Spinner size={14} />
                  <span class="grow">
                    Updating to <span class="mono">{target()}</span>. kuroe will be away for a few seconds, deployments
                    keep running.
                  </span>
                  <span class="mono faint">{elapsed()} s</span>
                </div>
              }
            >
              <Show when={message()}>
                <Note tone={phase() === "done" ? "ok" : "bad"}>{message()}</Note>
              </Show>
              <Show when={ready(latest)}>
                {(release) => (
                  <Show
                    when={release().newer}
                    fallback={<div class="hint">This is the latest release ({release().version}).</div>}
                  >
                    <Note
                      action={
                        <Show when={canUse(release(), v())}>
                          <Button size="sm" variant="primary" onClick={() => updateToRelease(release(), v())}>
                            Update to {release().version}
                          </Button>
                        </Show>
                      }
                    >
                      kuroe {release().version} is out. <ExtLink href={release().url}>Release notes</ExtLink>
                    </Note>
                  </Show>
                )}
              </Show>
              <Switch>
                <Match when={v().install.mode === "container" && v().install}>
                  {(install) => (
                    <details class="update-other">
                      <summary>Update to another version</summary>
                      <form class="hrow" onSubmit={(e) => onSubmit(e, v())}>
                        <Input
                          mono
                          required
                          class="grow"
                          label="Image"
                          placeholder="ghcr.io/owner/kuroe:1.2"
                          value={image() ?? (install() as { image: string }).image}
                          onValue={setImage}
                        />
                        <Button type="submit">Update</Button>
                      </form>
                    </details>
                  )}
                </Match>
                <Match when={v().install.mode === "binary"}>
                  <details class="update-other">
                    <summary>Update to another version</summary>
                    <form class="stack" style={{ gap: "12px" }} onSubmit={(e) => onSubmit(e, v())}>
                      <Field label="New binary URL">
                        <Input
                          mono
                          required
                          label="New binary URL"
                          placeholder="https://…/kuroe-linux-amd64"
                          value={binaryUrl()}
                          onValue={setBinaryUrl}
                        />
                      </Field>
                      <Field label="sha256" hint="Leave empty to read it from the URL plus .sha256.">
                        <Input mono label="sha256" value={sha256()} onValue={setSha256} />
                      </Field>
                      <div>
                        <Button type="submit">Update</Button>
                      </div>
                    </form>
                  </details>
                </Match>
                <Match when={v().install.mode === "unsupported"}>
                  <Note>Self-update works for the container install and the Linux binary.</Note>
                </Match>
              </Switch>
              <Show when={ready(status)?.log}>
                {(log) => (
                  <details class="update-other">
                    <summary>
                      Last updater log
                      {ready(status)?.exit_code != null ? ` (exit ${ready(status)?.exit_code})` : ""}
                    </summary>
                    <pre class="code">{log()}</pre>
                  </details>
                )}
              </Show>
            </Show>
          </div>
        </Card>
      )}
    </Show>
  );
}
