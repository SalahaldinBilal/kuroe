import { createMemo, createSignal, type JSX, onMount, Show } from "solid-js";
import { Icon } from "../Icon";
import { tooltip } from "../Tooltip/Tooltip";
import styles from "./Input.module.scss";

export type InputProps = {
  value: string | number;
  onValue?: (value: string) => void;
  type?: "text" | "password" | "number" | "search" | "email";
  placeholder?: string;
  disabled?: boolean;
  readOnly?: boolean;
  required?: boolean;
  pattern?: string;
  minLength?: number;
  maxLength?: number;
  inputMode?: "text" | "numeric" | "decimal" | "email" | "url";
  autocomplete?: string;
  label?: string;
  mono?: boolean;
  size?: "sm" | "md";
  autofocus?: boolean;
  clearable?: boolean;
  selectOnFocus?: boolean;
  min?: number;
  max?: number;
  before?: JSX.Element;
  after?: JSX.Element;
  class?: string;
  style?: JSX.CSSProperties;
  onKeyDown?: JSX.EventHandler<HTMLInputElement, KeyboardEvent>;
  onBlur?: JSX.EventHandler<HTMLInputElement, FocusEvent>;
  onPaste?: JSX.EventHandler<HTMLInputElement, ClipboardEvent>;
};

function Input(props: InputProps) {
  const [focused, setFocused] = createSignal(false);
  let input!: HTMLInputElement;

  const canClear = createMemo(
    () => (props.clearable ?? false) && String(props.value) !== "" && !props.disabled && !props.readOnly,
  );

  onMount(() => {
    if (props.autofocus) input.focus();
  });

  function clear() {
    props.onValue?.("");
    input.focus();
  }

  function onKeyDown(event: KeyboardEvent & { currentTarget: HTMLInputElement; target: Element }) {
    if (event.key === "Escape" && canClear()) {
      event.preventDefault();
      event.stopPropagation();
      clear();
      return;
    }
    props.onKeyDown?.(event);
  }

  return (
    // biome-ignore lint/a11y/noStaticElementInteractions: clicking the frame only moves focus to the input inside
    <div
      class={`${styles.Input} ${props.class ?? ""}`}
      classList={{
        [styles.Clearing]: canClear(),
        [styles.Focused]: focused(),
        [styles.Disabled]: props.disabled ?? false,
        [styles.Mono]: props.mono ?? false,
        [styles.Small]: props.size === "sm",
      }}
      style={props.style}
      onMouseDown={(event) => {
        if (event.target !== input && !(event.target as Element).closest("button")) {
          event.preventDefault();
          input.focus();
        }
      }}
    >
      {props.before}
      <input
        ref={input}
        type={props.type ?? "text"}
        value={props.value}
        placeholder={props.placeholder}
        disabled={props.disabled}
        readOnly={props.readOnly}
        required={props.required}
        pattern={props.pattern}
        minLength={props.minLength}
        maxLength={props.maxLength}
        inputMode={props.inputMode}
        aria-label={props.label}
        min={props.min}
        max={props.max}
        spellcheck={false}
        autocomplete={props.autocomplete ?? "off"}
        onInput={(event) => props.onValue?.(event.currentTarget.value)}
        onFocus={(event) => {
          setFocused(true);
          if (props.selectOnFocus) event.currentTarget.select();
        }}
        onBlur={(event) => {
          setFocused(false);
          props.onBlur?.(event);
        }}
        onKeyDown={onKeyDown}
        onPaste={(event) => props.onPaste?.(event)}
      />
      <Show when={canClear()}>
        <button
          type="button"
          class={styles.Clear}
          tabIndex={-1}
          aria-label="Clear"
          ref={(element) => tooltip(element, () => "Clear")}
          onClick={clear}
        >
          <Icon name="x" />
        </button>
      </Show>
      {props.after}
    </div>
  );
}

export function Unit(props: { children: JSX.Element }) {
  return <span class={styles.Unit}>{props.children}</span>;
}

export default Input;
