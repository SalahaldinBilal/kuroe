import { For, Show } from "solid-js";
import { Icon, type IconName } from "../Icon";
import styles from "./SegmentedControl.module.scss";

export type SegmentedOption<T> = {
  value: T;
  label: string;
  icon?: IconName;
  count?: number;
};

export type SegmentedControlProps<T> = {
  value: T;
  options: readonly SegmentedOption<T>[];
  onChange: (value: T) => void;
  fill?: boolean;
  label?: string;
};

function SegmentedControl<T>(props: SegmentedControlProps<T>) {
  return (
    // biome-ignore lint/a11y/useSemanticElements: a fieldset can't lay its options out as a flex row
    <div
      role="group"
      data-segmented
      class={styles.Control}
      classList={{ [styles.Fill]: props.fill ?? false }}
      aria-label={props.label}
    >
      <For each={props.options}>
        {(option) => (
          <button
            type="button"
            aria-pressed={option.value === props.value}
            class={styles.Option}
            classList={{ [styles.Selected]: option.value === props.value }}
            onClick={() => props.onChange(option.value)}
          >
            <Show when={option.icon}>{(icon) => <Icon name={icon()} />}</Show>
            {option.label}
            <Show when={option.count !== undefined}>
              <span class={styles.Count}>{option.count}</span>
            </Show>
          </button>
        )}
      </For>
    </div>
  );
}

export default SegmentedControl;
