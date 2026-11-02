import type { Resource } from "solid-js";
import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import useToastState from "../../states/toastState";
import Button from "./Button/Button";
import Field from "./Field/Field";
import { Icon } from "./Icon";
import Input from "./Input/Input";
import Spinner from "./Spinner/Spinner";

export function ready<T>(resource: Resource<T>): T | undefined {
  return resource.error ? undefined : resource();
}

// Deep equality for plain form drafts (objects, arrays, strings, numbers, booleans, null).
export function same(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

interface SaveBarProps {
  show: boolean;
  message: string;
  busy?: boolean;
  /** What saving is doing, shown with a spinner and the time so far while it runs. */
  busyMessage?: string;
  onSave: () => void;
  onDiscard?: () => void;
}

export function SaveBar(props: SaveBarProps) {
  const [seconds, setSeconds] = createSignal(0);
  createEffect(() => {
    if (!props.busy) return;
    setSeconds(0);
    const timer = setInterval(() => setSeconds((s) => s + 1), 1000);
    onCleanup(() => clearInterval(timer));
  });
  return (
    <Show when={props.show || props.busy}>
      <div class="savebar">
        <Show when={props.busy} fallback={<Icon name="info" />}>
          <Spinner />
        </Show>
        <span class="grow">
          <Show when={props.busy} fallback={props.message}>
            {props.busyMessage ?? "Saving"}
            <Show when={seconds() > 0}>
              <span class="savebar-time"> {seconds()}s</span>
            </Show>
          </Show>
        </span>
        <Show when={props.onDiscard && !props.busy}>
          <Button variant="ghost" size="sm" onClick={() => props.onDiscard?.()}>
            Discard
          </Button>
        </Show>
        <Button variant="primary" size="sm" disabled={props.busy} onClick={props.onSave}>
          {props.busy ? "Saving" : "Save changes"}
        </Button>
      </div>
    </Show>
  );
}

export function SecretField(props: { label: string; value: string; hint?: string }) {
  return (
    <Field label={props.label} hint={props.hint}>
      <div class="hrow">
        <Input mono readOnly selectOnFocus class="grow" label={props.label} value={props.value} />
        <CopyButton text={props.value} size="md" />
      </div>
    </Field>
  );
}

/** The clipboard API only exists on HTTPS and localhost, and kuroe is often opened over plain HTTP. */
export async function copyText(text: string): Promise<boolean> {
  if (navigator.clipboard && window.isSecureContext) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      return copyWithSelection(text);
    }
  }
  return copyWithSelection(text);
}

function copyWithSelection(text: string): boolean {
  const area = document.createElement("textarea");
  area.value = text;
  area.setAttribute("readonly", "");
  area.style.position = "fixed";
  area.style.opacity = "0";
  document.body.appendChild(area);
  area.select();
  const copied = document.execCommand("copy");
  area.remove();
  return copied;
}

export function CopyButton(props: { text: string; size?: "sm" | "md"; label?: string }) {
  const { pushToast } = useToastState;
  async function copy() {
    if (await copyText(props.text)) pushToast("Copied");
    else pushToast("Couldn't copy, select the text instead", "error");
  }
  return (
    <Button size={props.size ?? "sm"} onClick={copy}>
      <Icon name="copy" />
      {props.label ?? "Copy"}
    </Button>
  );
}
