import { type JSX, Show } from "solid-js";
import { Icon, type IconName } from "../Icon";
import Spinner from "../Spinner/Spinner";
import styles from "./EmptyState.module.scss";

export type EmptyStateProps = {
  icon?: IconName;
  title: JSX.Element;
  description?: JSX.Element;
  actions?: JSX.Element;
  tone?: "default" | "danger" | "success";
  loading?: boolean;
  compact?: boolean;
};

function EmptyState(props: EmptyStateProps) {
  return (
    <div
      class={styles.EmptyState}
      classList={{
        [styles.Danger]: props.tone === "danger",
        [styles.Success]: props.tone === "success",
        [styles.Compact]: props.compact ?? false,
      }}
    >
      <Show
        when={!props.loading}
        fallback={
          <div class={styles.Icon}>
            <Spinner size={20} />
          </div>
        }
      >
        <Show when={props.icon}>
          {(icon) => (
            <div class={styles.Icon}>
              <Icon name={icon()} />
            </div>
          )}
        </Show>
      </Show>
      <div class={styles.Title}>{props.title}</div>
      <Show when={props.description}>
        <div class={styles.Description}>{props.description}</div>
      </Show>
      <Show when={props.actions}>
        <div class={styles.Actions}>{props.actions}</div>
      </Show>
    </div>
  );
}

export function Loading(props: { label?: string }) {
  return <EmptyState loading compact title={props.label ?? "Loading"} />;
}

export default EmptyState;
