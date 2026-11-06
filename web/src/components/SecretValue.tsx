import { createSignal, type JSX, onMount, Show } from "solid-js";
import { useUser } from "../App";
import { api, can } from "../api";
import savedSecretState from "../states/savedSecretState";
import useToastState from "../states/toastState";
import { whitespaceProblem } from "../whitespace";
import Button from "./ui/Button/Button";
import EmptyState from "./ui/EmptyState/EmptyState";
import Field from "./ui/Field/Field";
import { Icon } from "./ui/Icon";
import Input from "./ui/Input/Input";
import Modal from "./ui/Modal/Modal";
import { ready } from "./ui/Overlay";
import Select from "./ui/Select/Select";
import Tip from "./ui/Tooltip/Tip";

const MATCH_MIN_LEN = 8;

export interface SecretChange {
  value: string;
  ref: string | null;
  secret: boolean;
}

interface Props {
  label: string;
  value: string;
  secretRef: string | null;
  /** A raw value that is secret shows as a password with a "raw" tag. */
  secret: boolean;
  placeholder?: string;
  /** Names from the deployment, for people who can't load the list. */
  names?: Record<string, string>;
  disabled?: boolean;
  onChange: (next: SecretChange) => void;
  onPaste?: JSX.EventHandler<HTMLInputElement, ClipboardEvent>;
}

function useSecretAccess() {
  const user = useUser();
  return {
    canUse: () => can(user(), "secret.use") || can(user(), "secret.manage"),
    canManage: () => can(user(), "secret.manage"),
  };
}

function secretName(id: string, names?: Record<string, string>): string {
  return ready(savedSecretState.list)?.find((s) => s.id === id)?.name ?? names?.[id] ?? "deleted secret";
}

export function SecretRef(props: { name: string; onUnlink?: () => void }) {
  return (
    <Tip content="Saved secret. Change its value on the Secrets page.">
      <div class="secret-ref grow">
        <Icon name="key" />
        <b>{props.name}</b>
        <span class="tag">saved</span>
        <span class="grow" />
        <Show when={props.onUnlink}>
          <button type="button" class="unlink" aria-label="Use a raw value instead" onClick={() => props.onUnlink?.()}>
            <Icon name="x" />
          </button>
        </Show>
      </div>
    </Tip>
  );
}

export function WhitespaceNote(props: { value: string; onTrim: (value: string) => void }) {
  const problem = () => whitespaceProblem(props.value);
  const trimmed = () => props.value.trim();
  return (
    <Show when={problem()}>
      {(text) => (
        <div class="ws-note">
          <Icon name="alert" />
          <span>{text()}.</span>
          <Show when={trimmed() !== props.value && trimmed() !== ""}>
            <button type="button" class="link" onClick={() => props.onTrim(trimmed())}>
              Remove it
            </button>
          </Show>
        </div>
      )}
    </Show>
  );
}

export function SecretValue(props: Props) {
  const access = useSecretAccess();
  const [picking, setPicking] = createSignal(false);
  const [matched, setMatched] = createSignal<{ name: string; raw: string } | null>(null);
  const checked = new Set<string>();
  const kept = new Set<string>();

  onMount(() => {
    if (access.canUse()) savedSecretState.load();
  });

  async function tryMatch(value: string) {
    if (!access.canUse() || props.disabled || props.secretRef || value.length < MATCH_MIN_LEN) return;
    if (checked.has(value) || kept.has(value)) return;
    checked.add(value);
    try {
      const hit = await api.matchSavedSecret(value);
      if (hit && !props.secretRef && props.value === value) {
        props.onChange({ value: "", ref: hit.id, secret: false });
        setMatched({ name: hit.name, raw: value });
      }
    } catch {
      // A failed or rate limited lookup leaves the raw value as it is.
    }
  }

  function keepRaw() {
    const m = matched();
    if (!m) return;
    kept.add(m.raw);
    props.onChange({ value: m.raw, ref: null, secret: true });
    setMatched(null);
  }

  function pick(id: string) {
    setMatched(null);
    props.onChange({ value: "", ref: id, secret: false });
    setPicking(false);
  }

  return (
    <div class="secret-value">
      <div class="hrow">
        <Show
          when={props.secretRef}
          fallback={
            <Input
              mono
              class="grow"
              label={props.label}
              placeholder={props.placeholder ?? "value"}
              type={props.secret ? "password" : "text"}
              showSpaces
              value={props.value}
              disabled={props.disabled}
              before={
                props.secret ? (
                  <Tip content="Raw value, stored only on this deployment">
                    <span class="raw-tag">raw</span>
                  </Tip>
                ) : undefined
              }
              onValue={(value) => props.onChange({ value, ref: null, secret: props.secret })}
              onPaste={(event) => {
                props.onPaste?.(event);
                if (event.defaultPrevented) return;
                const pasted = event.clipboardData?.getData("text") ?? "";
                if (!pasted.includes("\n")) setTimeout(() => tryMatch(props.value), 0);
              }}
              onBlur={() => tryMatch(props.value)}
            />
          }
        >
          {(id) => (
            <SecretRef
              name={secretName(id(), props.names)}
              onUnlink={
                props.disabled
                  ? undefined
                  : () => {
                      setMatched(null);
                      props.onChange({ value: "", ref: null, secret: true });
                    }
              }
            />
          )}
        </Show>
        <Show when={access.canUse() && !props.disabled}>
          <Button variant="ghost" icon size="sm" tooltip="Use a saved secret" onClick={() => setPicking(true)}>
            <Icon name="key" />
          </Button>
        </Show>
      </div>
      <Show when={!props.secretRef}>
        <WhitespaceNote
          value={props.value}
          onTrim={(value) => props.onChange({ value, ref: null, secret: props.secret })}
        />
      </Show>
      <Show when={matched()}>
        {(m) => (
          <div class="ref-note">
            <span>
              Matches the saved secret <b>{m().name}</b>, linked to it.
            </span>
            <button type="button" class="link" onClick={keepRaw}>
              Keep the raw value
            </button>
          </div>
        )}
      </Show>
      <PickSavedSecret
        show={picking()}
        raw={props.secretRef ? "" : props.value}
        current={props.secretRef}
        onPick={pick}
        onHide={() => setPicking(false)}
      />
    </div>
  );
}

function PickSavedSecret(props: {
  show: boolean;
  raw: string;
  current: string | null;
  onPick: (id: string) => void;
  onHide: () => void;
}) {
  const { pushError } = useToastState;
  const access = useSecretAccess();
  const [chosen, setChosen] = createSignal("");
  const [name, setName] = createSignal("");
  const [description, setDescription] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const list = () => ready(savedSecretState.list) ?? [];
  const selected = () => chosen() || props.current || list()[0]?.id || "";

  async function saveAndUse() {
    setBusy(true);
    try {
      const created = await api.createSavedSecret({
        name: name().trim(),
        description: description().trim(),
        value: props.raw,
      });
      await savedSecretState.refetch();
      setName("");
      setDescription("");
      props.onPick(created.id);
    } catch (err) {
      pushError(err);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal
      show={props.show}
      onHide={props.onHide}
      title="Use a saved secret"
      subtitle="The value comes from the Secrets page and stays in sync with it."
      footer={
        <>
          <Button variant="ghost" onClick={props.onHide}>
            Cancel
          </Button>
          <Button variant="primary" disabled={!selected()} onClick={() => props.onPick(selected())}>
            Use it
          </Button>
        </>
      }
    >
      <Show
        when={list().length > 0}
        fallback={
          <EmptyState
            compact
            icon="key"
            title="No saved secrets yet"
            description="Save one below or on the Secrets page."
          />
        }
      >
        <Field label="Saved secret">
          <Select
            mono
            label="Saved secret"
            items={list().map((s) => ({ value: s.id, label: s.name, hint: s.description || undefined }))}
            value={selected()}
            onChange={setChosen}
          />
        </Field>
      </Show>
      <Show when={access.canManage() && props.raw}>
        <div class="stack" style={{ gap: "10px" }}>
          <b>Or save this value as a new saved secret</b>
          <Field label="Name" hint="Lowercase letters, digits and dashes.">
            <Input mono label="Name" placeholder="db-password" value={name()} onValue={setName} />
          </Field>
          <Field label="Description">
            <Input label="Description" placeholder="What it's for" value={description()} onValue={setDescription} />
          </Field>
          <div class="hrow">
            <Button disabled={busy() || !name().trim()} onClick={saveAndUse}>
              <Icon name="plus" />
              Save and use
            </Button>
          </div>
        </div>
      </Show>
    </Modal>
  );
}
