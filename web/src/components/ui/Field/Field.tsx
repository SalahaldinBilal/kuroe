import { type JSX, Show } from "solid-js";
import styles from "./Field.module.scss";

export type FieldProps = {
  label: JSX.Element;
  hint?: JSX.Element;
  grow?: boolean;
  width?: number;
  children: JSX.Element;
};

function Field(props: FieldProps) {
  return (
    <div
      class={styles.Field}
      classList={{ [styles.Grow]: props.grow ?? false }}
      style={{ width: props.width ? `${props.width}px` : undefined }}
    >
      <span class={styles.Label}>{props.label}</span>
      {props.children}
      <Show when={props.hint}>
        <span class={styles.Hint}>{props.hint}</span>
      </Show>
    </div>
  );
}

export function FieldRow(props: { children: JSX.Element }) {
  return <div class={styles.Row}>{props.children}</div>;
}

export default Field;
