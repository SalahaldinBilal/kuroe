import type { JSX } from "solid-js";
import { createEffect, createSignal, For, Show } from "solid-js";
import Button from "./Button/Button";
import { Icon, type IconName } from "./Icon";
import Input, { Unit } from "./Input/Input";

export function cls(...parts: (string | false | null | undefined)[]): string {
  return parts.filter(Boolean).join(" ");
}

interface SliderProps {
  value: number | null;
  onChange: (value: number | null) => void;
  min: number;
  max: number;
  step: number;
  unit: string;
  fallback: number | null;
  label: string;
  disabled?: boolean;
}

export function Slider(props: SliderProps) {
  const [text, setText] = createSignal(props.value == null ? "" : String(props.value));
  createEffect(() => {
    const v = props.value;
    if (v == null ? text().trim() !== "" : Number(text()) !== v) setText(v == null ? "" : String(v));
  });
  const pos = () => props.value ?? props.fallback ?? props.max;
  const percent = () => Math.min(100, Math.max(0, ((pos() - props.min) / (props.max - props.min)) * 100));

  function fromRange(raw: string) {
    const v = Number(raw);
    props.onChange(props.fallback == null && v >= props.max ? null : v);
  }

  function fromText(raw: string) {
    setText(raw);
    const trimmed = raw.trim();
    if (!trimmed) return props.onChange(null);
    const v = Number(trimmed);
    if (Number.isFinite(v) && v > 0) props.onChange(v);
  }

  const empty = () => props.value == null && text().trim() === "";
  const clearHint = () =>
    props.fallback == null ? "Clear for no limit" : `Clear to use the default, ${props.fallback} ${props.unit}`;

  return (
    <div class="lim">
      <input
        type="range"
        aria-label={props.label}
        min={props.min}
        max={props.max}
        step={props.step}
        value={pos()}
        disabled={props.disabled}
        class={props.value == null ? "is-def" : ""}
        style={{ "--p": `${percent()}%` }}
        onInput={(e) => fromRange(e.currentTarget.value)}
      />
      <Input
        mono
        inputMode="decimal"
        label={`${props.label} value`}
        value={text()}
        placeholder={props.fallback == null ? "no limit" : String(props.fallback)}
        disabled={props.disabled}
        onValue={fromText}
        after={
          <Show when={!(empty() && props.fallback == null)}>
            <Unit>{props.unit}</Unit>
          </Show>
        }
      />
      <Button variant="ghost" tooltip={clearHint()} disabled={props.disabled || empty()} onClick={() => fromText("")}>
        Clear
      </Button>
    </div>
  );
}

export interface TileOption<T extends string> {
  value: T;
  label: JSX.Element;
  description?: JSX.Element;
  icon?: IconName;
}

interface TilesProps<T extends string> {
  options: readonly TileOption<T>[];
  value: T;
  onChange: (value: T) => void;
  columns?: 1 | 2 | 3 | 4;
  disabled?: boolean;
  label?: string;
}

export function Tiles<T extends string>(props: TilesProps<T>) {
  const columns = () => props.columns ?? 4;
  return (
    <fieldset class={cls("tiles", columns() !== 4 && `c${columns()}`)} aria-label={props.label}>
      <For each={props.options}>
        {(o) => (
          <button
            type="button"
            aria-pressed={o.value === props.value}
            class={cls("tile", o.value === props.value && "on")}
            disabled={props.disabled}
            onClick={() => props.onChange(o.value)}
          >
            <Show
              when={columns() !== 1}
              fallback={
                <>
                  <span class="radio" />
                  <span class="tt">
                    <b>{o.label}</b>
                    <Show when={o.description}>
                      <span class="d">{o.description}</span>
                    </Show>
                  </span>
                </>
              }
            >
              <span class="ti">
                <Show when={o.icon} fallback={<b>{o.label}</b>}>
                  {(name) => <Icon name={name()} />}
                </Show>
                <span class="radio" />
              </span>
              <Show when={o.icon}>
                <b>{o.label}</b>
              </Show>
              <Show when={o.description}>
                <span class="d">{o.description}</span>
              </Show>
            </Show>
          </button>
        )}
      </For>
    </fieldset>
  );
}
