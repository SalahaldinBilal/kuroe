import { A } from "@solidjs/router";
import { createResource, createSignal, For, onCleanup, Show } from "solid-js";
import { useUser } from "../App";
import {
  ApiError,
  api,
  type Identity,
  isOwner,
  type RunnerDocker,
  type RunnerInput,
  type RunnerPool,
  type RunnerProvider,
  type RunnerScope,
} from "../api";
import { Card, Note, PageHeader, Pill } from "../components/ui/Blocks";
import Button from "../components/ui/Button/Button";
import EmptyState from "../components/ui/EmptyState/EmptyState";
import Field, { FieldRow } from "../components/ui/Field/Field";
import { Slider, Tiles } from "../components/ui/Form";
import { Icon } from "../components/ui/Icon";
import Input, { Unit } from "../components/ui/Input/Input";
import Modal from "../components/ui/Modal/Modal";
import { ready } from "../components/ui/Overlay";
import SegmentedControl from "../components/ui/SegmentedControl/SegmentedControl";
import Select from "../components/ui/Select/Select";
import { Busy, SkeletonEmpty } from "../components/ui/Skeleton";
import Toggle from "../components/ui/Switch/Switch";
import { formatAgo } from "../format";
import { mbitText, parseMbit } from "../spec";
import { EVENT_DEBOUNCE_MS, onEvent } from "../states/eventState";
import useToastState, { errorMessage } from "../states/toastState";

const PROVIDERS: { value: RunnerProvider; label: string; description: string; kinds: string[] }[] = [
  {
    value: "github",
    label: "GitHub Actions",
    description: "One job per runner, started fresh each time.",
    kinds: ["github_app", "github_token"],
  },
  { value: "gitlab", label: "GitLab CI", description: "Long-running runners that pick up jobs.", kinds: ["gitlab"] },
  { value: "gitea", label: "Gitea or Forgejo Actions", description: "act_runner, long-running.", kinds: ["gitea"] },
];

const SCOPE_LABEL: Record<RunnerProvider, Record<RunnerScope, string>> = {
  github: { repo: "Repository", org: "Organization", instance: "" },
  gitlab: { repo: "Project", org: "Group", instance: "Whole instance" },
  gitea: { repo: "Repository", org: "Organization", instance: "Whole instance" },
};

const DOCKER: { value: RunnerDocker; label: string; icon: "box" | "alert" | "x"; description: string }[] = [
  {
    value: "dind",
    label: "Own Docker",
    icon: "box",
    description: "A Docker-in-Docker sidecar per runner. Jobs can't see your deployments. It runs privileged.",
  },
  {
    value: "host",
    label: "This server's Docker",
    icon: "alert",
    description: "Builds land straight in this server's Docker. Root on this machine. Owner only.",
  },
  { value: "none", label: "No Docker", icon: "x", description: "For pipelines that only test and lint." },
];

function blank(provider: RunnerProvider): RunnerInput {
  return {
    name: "",
    provider,
    identity: "",
    scope_kind: provider === "github" ? "repo" : "org",
    scope: "",
    labels: [],
    min_idle: 1,
    max: 2,
    cpus: null,
    memory_mb: null,
    net_in_mbit: null,
    net_out_mbit: null,
    docker_mode: "dind",
    image: null,
    enabled: true,
  };
}

function inputOf(pool: RunnerPool): RunnerInput {
  const { id: _id, last_error: _e, created_by: _c, created_at: _t, instances: _i, default_image: _d, ...input } = pool;
  return input;
}

export function Runners() {
  const user = useUser();
  const { pushToast, pushError } = useToastState;
  const [pools, { refetch }] = createResource(api.runners);
  const [identities] = createResource(api.identities);
  const [editing, setEditing] = createSignal<{ id: string | null; input: RunnerInput } | null>(null);
  const [removing, setRemoving] = createSignal<RunnerPool | null>(null);
  const [logs, setLogs] = createSignal<{ title: string; text: string } | null>(null);
  const [clearing, setClearing] = createSignal<RunnerPool | null>(null);
  const timer = setInterval(() => refetch(), 10_000);
  onCleanup(() => clearInterval(timer));
  onEvent((event) => event.kind === "runner.instance", refetch, EVENT_DEBOUNCE_MS);

  async function clearCache(pool: RunnerPool) {
    try {
      const { removed } = await api.clearRunnerCache(pool.id);
      setClearing(null);
      pushToast(`Cleared ${removed} build ${removed === 1 ? "cache" : "caches"} of ${pool.name}`);
      refetch();
    } catch (err) {
      pushError(err);
    }
  }

  async function remove(pool: RunnerPool) {
    try {
      const { leftovers } = await api.removeRunners(pool.id);
      setRemoving(null);
      if (leftovers.length) {
        pushToast(`Removed ${pool.name}. Remove these at the provider by hand: ${leftovers.join("; ")}`, "error");
      } else {
        pushToast(`Removed ${pool.name}`);
      }
      refetch();
    } catch (err) {
      pushError(err);
    }
  }

  async function showLogs(pool: RunnerPool, instance: string) {
    try {
      setLogs({ title: instance, text: await api.runnerLogs(pool.id, instance) });
    } catch (err) {
      pushError(err);
    }
  }

  const list = () => ready(pools) ?? [];
  const addButton = (
    <Button variant="primary" size="sm" onClick={() => setEditing({ id: null, input: blank("github") })}>
      <Icon name="plus" />
      Add runners
    </Button>
  );

  return (
    <div class="content narrow">
      <PageHeader
        title="Runners"
        description="CI runners on this server that GitHub, GitLab, Gitea or Forgejo send jobs to"
        actions={addButton}
      />
      <Show
        when={list().length > 0}
        fallback={
          <Card>
            <Show
              when={pools.latest || pools.error}
              fallback={
                <Busy>
                  <SkeletonEmpty action lines={2} />
                </Busy>
              }
            >
              <EmptyState
                icon="play"
                title="No runners yet"
                description="Builds then run on this server, with no hosted CI minutes and no registry round trip."
                actions={addButton}
              />
            </Show>
          </Card>
        }
      >
        <div class="stack">
          <For each={list()}>
            {(pool) => {
              const provider = () => PROVIDERS.find((p) => p.value === pool.provider);
              const running = () => pool.instances.filter((i) => i.state === "running");
              const busy = () => running().filter((i) => i.busy).length;
              return (
                <Card
                  title={pool.name}
                  description={`${provider()?.label}, ${pool.scope || "whole instance"}`}
                  actions={
                    <>
                      <Pill tone={pool.enabled ? (pool.last_error ? "bad" : "ok") : undefined} dot>
                        {pool.enabled ? `${running().length} running${busy() ? `, ${busy()} busy` : ""}` : "off"}
                      </Pill>
                      <Show when={pool.docker_mode === "dind"}>
                        <Button size="sm" variant="ghost" onClick={() => setClearing(pool)}>
                          Clear caches
                        </Button>
                      </Show>
                      <Button size="sm" onClick={() => setEditing({ id: pool.id, input: inputOf(pool) })}>
                        Edit
                      </Button>
                      <Button variant="ghost" icon size="sm" tooltip="Remove" onClick={() => setRemoving(pool)}>
                        <Icon name="trash" />
                      </Button>
                    </>
                  }
                >
                  <div class="card-b stack" style={{ gap: "10px" }}>
                    <Show when={pool.last_error}>{(error) => <Note tone="bad">{error()}</Note>}</Show>
                    <div class="hrow wrap" style={{ gap: "6px" }}>
                      <For each={["kuroe", pool.name, ...pool.labels.filter((l) => l !== "kuroe" && l !== pool.name)]}>
                        {(label) => <span class="chip">{label}</span>}
                      </For>
                      <span class="faint">
                        {pool.provider === "github"
                          ? `keeps ${pool.min_idle} idle, at most ${pool.max}`
                          : `keeps ${pool.max}`}
                      </span>
                    </div>
                    <For each={pool.instances} fallback={<span class="faint">No runners started yet.</span>}>
                      {(instance) => (
                        <div class="hrow" style={{ gap: "8px" }}>
                          <span class="mono grow">{instance.name}</span>
                          <Show when={instance.busy}>
                            <Pill tone="warn">busy</Pill>
                          </Show>
                          <span class="muted">{instance.state}</span>
                          <span class="faint hide-sm">{formatAgo(instance.created)}</span>
                          <Button size="sm" variant="ghost" onClick={() => showLogs(pool, instance.name)}>
                            <Icon name="logs" />
                            Logs
                          </Button>
                        </div>
                      )}
                    </For>
                  </div>
                </Card>
              );
            }}
          </For>
        </div>
      </Show>

      <Show when={editing()}>
        {(e) => (
          <PoolDialog
            id={e().id}
            initial={e().input}
            identities={ready(identities) ?? []}
            owner={isOwner(user())}
            onClose={() => setEditing(null)}
            onSaved={() => {
              setEditing(null);
              refetch();
            }}
          />
        )}
      </Show>

      <Modal
        show={removing() !== null}
        title={`Remove ${removing()?.name}?`}
        onHide={() => setRemoving(null)}
        footer={
          <>
            <Button variant="ghost" onClick={() => setRemoving(null)}>
              Cancel
            </Button>
            <Button variant="danger-solid" onClick={() => removing() && remove(removing() as RunnerPool)}>
              Remove
            </Button>
          </>
        }
      >
        <p>Its runners stop, including any job they're running, and kuroe removes them at the provider.</p>
      </Modal>

      <Modal
        show={clearing() !== null}
        title={`Clear the build caches of ${clearing()?.name}?`}
        onHide={() => setClearing(null)}
        footer={
          <>
            <Button variant="ghost" onClick={() => setClearing(null)}>
              Cancel
            </Button>
            <Button variant="danger-solid" onClick={() => clearing() && clearCache(clearing() as RunnerPool)}>
              Clear caches
            </Button>
          </>
        }
      >
        <p>
          The runners stop, including any job they're running, and their Docker images and build caches are deleted. New
          runners start right after with empty caches.
        </p>
      </Modal>

      <Modal show={logs() !== null} width={760} title={logs()?.title} onHide={() => setLogs(null)}>
        <pre class="code" style={{ "max-height": "60vh", overflow: "auto" }}>
          {logs()?.text || "Nothing written yet."}
        </pre>
      </Modal>
    </div>
  );
}

function PoolDialog(props: {
  id: string | null;
  initial: RunnerInput;
  identities: Identity[];
  owner: boolean;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { pushToast } = useToastState;
  const [input, setInput] = createSignal<RunnerInput>(props.initial);
  const [labels, setLabels] = createSignal(props.initial.labels.join(", "));
  const [netIn, setNetIn] = createSignal(mbitText(props.initial.net_in_mbit));
  const [netOut, setNetOut] = createSignal(mbitText(props.initial.net_out_mbit));
  const [error, setError] = createSignal("");
  const [publicWarning, setPublicWarning] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const set = (patch: Partial<RunnerInput>) => setInput({ ...input(), ...patch });
  const provider = () => PROVIDERS.find((p) => p.value === input().provider);
  const logins = () => props.identities.filter((i) => provider()?.kinds.includes(i.kind));
  const scopes = (): RunnerScope[] => (input().provider === "github" ? ["repo", "org"] : ["repo", "org", "instance"]);

  async function save(confirmPublic = false) {
    setError("");
    setBusy(true);
    try {
      const body = {
        ...input(),
        labels: labels()
          .split(",")
          .map((l) => l.trim())
          .filter(Boolean),
        net_in_mbit: parseMbit(netIn(), "Download limit"),
        net_out_mbit: parseMbit(netOut(), "Upload limit"),
      };
      if (props.id) {
        await api.updateRunners(props.id, body);
        pushToast(`Saved ${body.name}`);
      } else {
        await api.createRunners(body, confirmPublic);
        pushToast(`Added ${body.name}. Runners start in a few seconds.`);
      }
      props.onSaved();
    } catch (err) {
      if (err instanceof ApiError && err.status === 409 && err.message.includes("public")) {
        setPublicWarning(err.message);
      } else {
        setError(errorMessage(err));
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal
      show
      width={640}
      title={props.id ? `Edit ${props.initial.name}` : "Add runners"}
      onHide={props.onClose}
      footer={
        <>
          <Button variant="ghost" onClick={props.onClose}>
            Cancel
          </Button>
          <Button variant="primary" disabled={busy()} onClick={() => save()}>
            {props.id ? "Save" : "Add runners"}
          </Button>
        </>
      }
    >
      <div class="stack" style={{ gap: "14px" }}>
        <Show when={!props.id}>
          <Tiles
            label="Provider"
            columns={3}
            value={input().provider}
            onChange={(p) => setInput({ ...blank(p), name: input().name })}
            options={PROVIDERS.map((p) => ({ value: p.value, label: p.label, description: p.description }))}
          />
          <Field
            label="Identity"
            hint={
              <>
                Registers the runners. Add one under <A href="/identities">Identities</A>.
                {input().provider === "github"
                  ? " A GitHub App needs Self-hosted runners: write, a token Administration: write."
                  : ""}
              </>
            }
          >
            <Select
              label="Identity"
              placeholder={logins().length ? "Pick an identity" : "Add an identity first"}
              items={logins().map((i) => ({ value: i.id, label: i.name }))}
              value={input().identity}
              onChange={(identity) => set({ identity })}
            />
          </Field>
          <Field label="Serves">
            <div>
              <SegmentedControl
                label="Serves"
                value={input().scope_kind}
                onChange={(scope_kind) => set({ scope_kind })}
                options={scopes().map((s) => ({ value: s, label: SCOPE_LABEL[input().provider][s] }))}
              />
            </div>
          </Field>
          <Show when={input().scope_kind !== "instance"}>
            <Field label={SCOPE_LABEL[input().provider][input().scope_kind]}>
              <Input
                mono
                label="Scope"
                placeholder={input().scope_kind === "repo" ? "acme/api" : "acme"}
                value={input().scope}
                onValue={(scope) => set({ scope })}
              />
            </Field>
          </Show>
        </Show>
        <FieldRow>
          <Field label="Name" hint="Also a label jobs can ask for.">
            <Input mono label="Name" placeholder="builds" value={input().name} onValue={(name) => set({ name })} />
          </Field>
          <Field label="More labels" grow hint="Comma separated. kuroe and the name are always added.">
            <Input mono label="Labels" placeholder="linux, big" value={labels()} onValue={setLabels} />
          </Field>
        </FieldRow>
        <FieldRow>
          <Show when={input().provider === "github"}>
            <Field label="Idle runners" hint="Ready before a job arrives.">
              <Input
                mono
                inputMode="numeric"
                label="Idle runners"
                value={String(input().min_idle)}
                onValue={(v) => set({ min_idle: Number(v) || 0 })}
              />
            </Field>
          </Show>
          <Field label={input().provider === "github" ? "At most" : "Runners"}>
            <Input
              mono
              inputMode="numeric"
              label="Most runners"
              value={String(input().max)}
              onValue={(v) => set({ max: Number(v) || 0 })}
            />
          </Field>
        </FieldRow>
        <Field label="CPUs per runner">
          <Slider
            label="CPUs per runner"
            value={input().cpus}
            onChange={(cpus) => set({ cpus })}
            min={0.5}
            max={16}
            step={0.5}
            unit="cores"
            fallback={null}
          />
        </Field>
        <Field label="Memory per runner">
          <Slider
            label="Memory per runner"
            value={input().memory_mb}
            onChange={(memory_mb) => set({ memory_mb })}
            min={256}
            max={32768}
            step={256}
            unit="MB"
            fallback={null}
          />
        </Field>
        <FieldRow>
          <Field label="Download limit" grow>
            <Input
              mono
              inputMode="decimal"
              label="Download limit"
              placeholder="No limit"
              value={netIn()}
              onValue={setNetIn}
              after={<Unit>Mbit/s</Unit>}
            />
          </Field>
          <Field label="Upload limit" grow>
            <Input
              mono
              inputMode="decimal"
              label="Upload limit"
              placeholder="No limit"
              value={netOut()}
              onValue={setNetOut}
              after={<Unit>Mbit/s</Unit>}
            />
          </Field>
        </FieldRow>
        <span class="hint">
          {input().docker_mode === "host"
            ? "Per runner. Containers that jobs start on this server's Docker aren't limited."
            : "Per runner, and for its Docker sidecar too. Takes effect for runners started after saving."}
        </span>
        <Field label="Docker in jobs">
          <Tiles
            label="Docker in jobs"
            columns={3}
            value={input().docker_mode}
            onChange={(docker_mode) => set({ docker_mode })}
            options={DOCKER}
          />
        </Field>
        <Show when={input().docker_mode === "host" && !props.owner}>
          <Note tone="warn">Only the owner can give jobs this server's Docker.</Note>
        </Show>
        <Field label="Runner image" hint="Leave empty for the provider's official image.">
          <Input
            mono
            label="Runner image"
            placeholder={
              input().provider === "github"
                ? "ghcr.io/actions/actions-runner"
                : input().provider === "gitlab"
                  ? "gitlab/gitlab-runner:alpine"
                  : "gitea/act_runner"
            }
            value={input().image ?? ""}
            onValue={(image) => set({ image: image.trim() || null })}
          />
        </Field>
        <Show when={props.id}>
          <Toggle label="Runners on" checked={input().enabled} onChange={(enabled) => set({ enabled })} />
        </Show>
        <Show when={publicWarning()}>
          <Note
            tone="warn"
            action={
              <Button size="sm" variant="danger" disabled={busy()} onClick={() => save(true)}>
                Add anyway
              </Button>
            }
          >
            {publicWarning()}
          </Note>
        </Show>
        <Show when={error()}>
          <Note tone="bad">{error()}</Note>
        </Show>
      </div>
    </Modal>
  );
}
