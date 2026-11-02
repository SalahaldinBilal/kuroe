import { createResource, createSignal, Match, onCleanup, Show, Switch } from "solid-js";
import { api, type LatestRelease, type VersionInfo } from "../api";
import { errorMessage } from "../states/toastState";
import { Card, Note } from "./ui/Blocks";
import Button from "./ui/Button/Button";
import Field from "./ui/Field/Field";
import Input from "./ui/Input/Input";
import { ready } from "./ui/Overlay";
import { Bone, Busy, SkeletonCardHead } from "./ui/Skeleton";

const POLL_MS = 1000;
const GIVE_UP_MS = 120_000;

function label(v: VersionInfo): string {
  return v.build ? `${v.version} (${v.build})` : v.version;
}

export function SelfUpdate() {
  const [current, { refetch }] = createResource(api.version);
  const [status, { refetch: refetchStatus }] = createResource(api.updateStatus);
  const [latest] = createResource(api.latestRelease);
  const [phase, setPhase] = createSignal<"idle" | "updating" | "done" | "failed">("idle");
  const [message, setMessage] = createSignal("");
  const [image, setImage] = createSignal<string | null>(null);
  const [binaryUrl, setBinaryUrl] = createSignal("");
  const [sha256, setSha256] = createSignal("");
  let timer: number | undefined;
  onCleanup(() => clearInterval(timer));

  function watch(before: string) {
    const started = Date.now();
    let wentAway = false;
    timer = window.setInterval(async () => {
      try {
        const now = await api.version();
        if (label(now) !== before) {
          clearInterval(timer);
          setPhase("done");
          setMessage(`Now running ${label(now)}. Back after ${Math.round((Date.now() - started) / 1000)} s.`);
          refetch();
          refetchStatus();
          return;
        }
        if (wentAway) {
          const s = await api.updateStatus();
          if (s.state === "exited") {
            clearInterval(timer);
            setPhase(s.exit_code === 0 ? "done" : "failed");
            setMessage(s.exit_code === 0 ? "Update finished." : `The update failed and ${before} is running again.`);
            refetchStatus();
          }
        }
      } catch {
        wentAway = true;
      }
      if (Date.now() - started > GIVE_UP_MS) {
        clearInterval(timer);
        setPhase("failed");
        setMessage("No answer for two minutes. Check the updater log below.");
        refetchStatus();
      }
    }, POLL_MS);
  }

  function applyRelease(release: LatestRelease, v: VersionInfo) {
    if (v.install.mode === "container") {
      setImage(release.image);
    } else if (release.binary_url) {
      setBinaryUrl(release.binary_url);
      setSha256("");
    }
  }

  function canUse(release: LatestRelease, v: VersionInfo): boolean {
    return v.install.mode === "container" || (v.install.mode === "binary" && release.binary_url !== null);
  }

  async function onSubmit(event: SubmitEvent, v: VersionInfo) {
    event.preventDefault();
    const optional = (value: string) => value.trim() || undefined;
    setPhase("updating");
    setMessage("Updating. kuroe will be away for a few seconds, deployments keep running.");
    try {
      await api.startUpdate(
        v.install.mode === "container"
          ? { image: optional(image() ?? v.install.image) }
          : { binary_url: optional(binaryUrl()), sha256: optional(sha256()) },
      );
      watch(label(v));
    } catch (err) {
      setPhase("failed");
      setMessage(errorMessage(err));
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
            <Show when={ready(latest)}>
              {(release) => (
                <Show
                  when={release().newer}
                  fallback={<div class="hint">This is the latest release ({release().version}).</div>}
                >
                  <Note
                    action={
                      <Show when={canUse(release(), v())}>
                        <Button size="sm" onClick={() => applyRelease(release(), v())}>
                          Use it
                        </Button>
                      </Show>
                    }
                  >
                    kuroe {release().version} is out.{" "}
                    <a href={release().url} target="_blank" rel="noreferrer">
                      Release notes
                    </a>
                  </Note>
                </Show>
              )}
            </Show>
            <Switch>
              <Match when={v().install.mode === "container" && v().install}>
                {(install) => (
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
                    <Button type="submit" variant="primary" disabled={phase() === "updating"}>
                      Update
                    </Button>
                  </form>
                )}
              </Match>
              <Match when={v().install.mode === "binary"}>
                <form class="stack" style={{ gap: "12px" }} onSubmit={(e) => onSubmit(e, v())}>
                  <Field label="New binary URL">
                    <Input
                      mono
                      required
                      label="New binary URL"
                      placeholder="https://…/kuroe-x86_64-linux"
                      value={binaryUrl()}
                      onValue={setBinaryUrl}
                    />
                  </Field>
                  <Field label="sha256" hint="Leave empty to read it from the URL plus .sha256.">
                    <Input mono label="sha256" value={sha256()} onValue={setSha256} />
                  </Field>
                  <div>
                    <Button type="submit" variant="primary" disabled={phase() === "updating"}>
                      Update
                    </Button>
                  </div>
                </form>
              </Match>
              <Match when={v().install.mode === "unsupported"}>
                <Note>Self-update works for the container install and the Linux binary.</Note>
              </Match>
            </Switch>
            <Show when={message()}>
              <Note tone={phase() === "done" ? "ok" : phase() === "failed" ? "bad" : undefined}>{message()}</Note>
            </Show>
            <Show when={ready(status)?.log}>
              {(log) => (
                <details>
                  <summary class="hint" style={{ cursor: "pointer" }}>
                    Last updater log
                    {ready(status)?.exit_code != null ? ` (exit ${ready(status)?.exit_code})` : ""}
                  </summary>
                  <pre class="code" style={{ "margin-top": "8px" }}>
                    {log()}
                  </pre>
                </details>
              )}
            </Show>
          </div>
        </Card>
      )}
    </Show>
  );
}
