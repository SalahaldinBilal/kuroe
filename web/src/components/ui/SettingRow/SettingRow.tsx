import { type JSX, Show } from "solid-js";
import styles from "./SettingRow.module.scss";

// One setting inside a card: label and description on the left, control on the right. `stack` rows put the control
// below the text at full width.
function SettingRow(props: {
  label: JSX.Element;
  description?: JSX.Element;
  type?: "inline" | "stack";
  disabled?: boolean;
  children: JSX.Element;
}) {
  return (
    <div
      class={styles.Row}
      classList={{
        [styles.Stack]: props.type === "stack",
        [styles.Disabled]: props.disabled ?? false,
      }}
    >
      <div class={styles.Text}>
        <div class={styles.Label}>{props.label}</div>
        <Show when={props.description}>
          <div class={styles.Description}>{props.description}</div>
        </Show>
      </div>
      <div class={styles.Control}>{props.children}</div>
    </div>
  );
}

export default SettingRow;
