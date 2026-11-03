import { createEffect, createMemo, createSignal, For, type JSX, onCleanup, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { keepInViewport } from "../../../helpers/viewport";
import { Icon } from "../Icon";
import styles from "./Select.module.scss";

export type SelectItem<T> = {
  value: T;
  label: string;
  hint?: string;
  disabled?: boolean;
};

export type SelectProps<T> = {
  value: T;
  items: readonly SelectItem<T>[];
  onChange: (value: T) => void;
  placeholder?: string;
  disabled?: boolean;
  size?: "sm" | "md";
  mono?: boolean;
  label?: string;
  style?: JSX.CSSProperties;
  class?: string;
  /** Typing while the menu is open filters it, for long lists. */
  searchable?: boolean;
};

const MENU_MARGIN = 4;
const MENU_MAX_HEIGHT = 300;

export function items(values: readonly string[]): SelectItem<string>[] {
  return values.map((value) => ({ value, label: value }));
}

function Select<T>(props: SelectProps<T>) {
  const [open, setOpen] = createSignal(false);
  const [highlight, setHighlight] = createSignal(0);
  const [menuStyle, setMenuStyle] = createSignal<JSX.CSSProperties>({});
  const [query, setQuery] = createSignal("");
  const selected = createMemo(() => props.items.find((item) => item.value === props.value));
  const shown = createMemo(() => {
    const needle = query().trim().toLowerCase();
    if (!needle) return props.items;
    return props.items.filter(
      (item) => item.label.toLowerCase().includes(needle) || String(item.value).toLowerCase().includes(needle),
    );
  });
  const searchRow = () => (props.searchable ? 1 : 0);

  let control!: HTMLDivElement;
  let menu: HTMLDivElement | undefined;
  let followHighlight = false;

  function openMenu() {
    if (props.disabled) return;

    const rect = control.getBoundingClientRect();
    const spaceBelow = window.innerHeight - rect.bottom - MENU_MARGIN;
    const upwards = spaceBelow < Math.min(MENU_MAX_HEIGHT, 160) && rect.top > spaceBelow;

    setMenuStyle({
      left: `${rect.left}px`,
      "min-width": `${rect.width}px`,
      ...(upwards
        ? { bottom: `${window.innerHeight - rect.top + MENU_MARGIN}px` }
        : { top: `${rect.bottom + MENU_MARGIN}px` }),
    });
    followHighlight = true;
    setQuery("");
    const current = props.items.findIndex((item) => item.value === props.value && !item.disabled);
    setHighlight(current >= 0 ? current : nextEnabled(-1, 1));
    setOpen(true);
  }

  function filter(next: string) {
    setQuery(next);
    followHighlight = true;
    setHighlight(nextEnabled(-1, 1));
  }

  function choose(item: SelectItem<T>) {
    if (item.disabled) return;
    props.onChange(item.value);
    setOpen(false);
  }

  // The nearest enabled item from `start` in `step` direction, or `start` when there is none.
  function nextEnabled(start: number, step: number): number {
    const list = shown();
    for (let index = start + step; index >= 0 && index < list.length; index += step) {
      if (!list[index].disabled) return index;
    }
    return start;
  }

  // The menu is positioned against the viewport, so any outside scroll or resize would detach it.
  createEffect(() => {
    if (!open()) return;

    function close(event: Event) {
      if (menu && event.target instanceof Node && menu.contains(event.target)) return;
      setOpen(false);
    }

    window.addEventListener("scroll", close, { capture: true });
    window.addEventListener("resize", close);
    onCleanup(() => {
      window.removeEventListener("scroll", close, { capture: true });
      window.removeEventListener("resize", close);
    });
  });

  createEffect(() => {
    const index = highlight();
    if (!open() || !menu || !followHighlight) return;

    followHighlight = false;
    const item = menu.children[index + searchRow()] as HTMLElement | undefined;
    if (!item) return;

    if (item.offsetTop < menu.scrollTop) {
      menu.scrollTop = item.offsetTop;
    } else if (item.offsetTop + item.offsetHeight > menu.scrollTop + menu.clientHeight) {
      menu.scrollTop = item.offsetTop + item.offsetHeight - menu.clientHeight;
    }
  });

  function onKeyDown(event: KeyboardEvent) {
    if (!open()) {
      if (["Enter", " ", "ArrowDown", "ArrowUp"].includes(event.key)) {
        event.preventDefault();
        openMenu();
      }
      return;
    }

    if (props.searchable && !event.ctrlKey && !event.metaKey && !event.altKey) {
      const typing = event.key.length === 1 && !(event.key === " " && query() === "");
      if (typing || (event.key === "Backspace" && query())) {
        event.preventDefault();
        filter(typing ? query() + event.key : query().slice(0, -1));
        return;
      }
    }

    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        followHighlight = true;
        setHighlight((index) => nextEnabled(index, 1));
        return;
      case "ArrowUp":
        event.preventDefault();
        followHighlight = true;
        setHighlight((index) => nextEnabled(index, -1));
        return;
      case "Enter":
      case " ": {
        event.preventDefault();
        const item = shown()[highlight()];
        if (item) choose(item);
        return;
      }
      case "Escape":
        event.preventDefault();
        event.stopPropagation();
        setOpen(false);
        return;
      case "Tab":
        setOpen(false);
        return;
    }
  }

  return (
    <>
      <div
        ref={control}
        tabIndex={props.disabled ? -1 : 0}
        role="combobox"
        aria-label={props.label}
        aria-expanded={open()}
        aria-haspopup="listbox"
        aria-controls="select-menu"
        class={`${styles.Select} ${props.class ?? ""}`}
        classList={{
          [styles.Open]: open(),
          [styles.Small]: props.size === "sm",
          [styles.Mono]: props.mono ?? false,
          [styles.Disabled]: props.disabled ?? false,
        }}
        style={props.style}
        onClick={() => (open() ? setOpen(false) : openMenu())}
        onKeyDown={onKeyDown}
        onFocusOut={() => setOpen(false)}
      >
        <Show when={selected()} fallback={<span class={styles.Placeholder}>{props.placeholder ?? "Select"}</span>}>
          {(item) => <span class={styles.Label}>{item().label}</span>}
        </Show>
        <Icon name="down" class={styles.Chevron} />
      </div>
      <Show when={open()}>
        <Portal>
          <div
            id="select-menu"
            ref={(element) => {
              menu = element;
              keepInViewport(element);
            }}
            role="listbox"
            tabIndex={-1}
            class={styles.Menu}
            classList={{ [styles.Mono]: props.mono ?? false }}
            style={menuStyle()}
            onMouseDown={(event) => event.preventDefault()}
          >
            <Show when={props.searchable}>
              <div class={styles.Search} classList={{ [styles.Typed]: query() !== "" }}>
                <Icon name="search" />
                <span>{query() || "Type to filter"}</span>
              </div>
            </Show>
            <Show when={shown().length === 0}>
              <div class={`${styles.Item} ${styles.ItemDisabled}`}>No matches</div>
            </Show>
            <For each={shown()}>
              {(item, index) => (
                <div
                  role="option"
                  tabIndex={-1}
                  aria-selected={item.value === props.value}
                  aria-disabled={item.disabled ?? false}
                  class={styles.Item}
                  classList={{
                    [styles.Highlighted]: index() === highlight() && !item.disabled,
                    [styles.ItemDisabled]: item.disabled ?? false,
                  }}
                  onMouseEnter={() => {
                    if (!item.disabled) setHighlight(index());
                  }}
                  onClick={() => choose(item)}
                  onKeyDown={() => {}}
                >
                  <span class={styles.Check}>
                    <Show when={item.value === props.value}>
                      <Icon name="check" />
                    </Show>
                  </span>
                  <span class={styles.ItemLabel}>{item.label}</span>
                  <Show when={item.hint}>
                    <span class={styles.Hint}>{item.hint}</span>
                  </Show>
                </div>
              )}
            </For>
          </div>
        </Portal>
      </Show>
    </>
  );
}

export default Select;
