import { createEffect, For, type JSX, on, Show } from "solid-js";
import styles from "./Tabs.module.scss";

export type TabOption<T> = { value: T; label: string; count?: number; disabled?: boolean };

export type TabsProps<T> = {
  tabs: readonly TabOption<T>[];
  value: T;
  onChange: (value: T) => void;
  class?: string;
  after?: JSX.Element;
};

// Text tabs with an underline on the active one. The row scrolls sideways, without a visible scrollbar, when the tabs don't fit.
function Tabs<T>(props: TabsProps<T>) {
  const tabRefs = new Map<T, HTMLButtonElement>();

  createEffect(
    on(
      () => props.value,
      (value) => {
        tabRefs.get(value)?.scrollIntoView({ block: "nearest", inline: "nearest", behavior: "smooth" });
      },
      { defer: true },
    ),
  );

  return (
    <div class={`${styles.Container} ${props.class ?? ""}`} role="tablist">
      <For each={props.tabs}>
        {(tab) => (
          <button
            type="button"
            ref={(element) => tabRefs.set(tab.value, element)}
            class={styles.Tab}
            classList={{ [styles.Active]: tab.value === props.value }}
            role="tab"
            aria-selected={tab.value === props.value}
            disabled={tab.disabled}
            onClick={() => tab.value !== props.value && props.onChange(tab.value)}
          >
            {tab.label}
            <Show when={tab.count !== undefined}>
              <span class={styles.Count}>{tab.count}</span>
            </Show>
          </button>
        )}
      </For>
      {props.after}
    </div>
  );
}

export default Tabs;
