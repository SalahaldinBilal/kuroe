import { For, Match, Switch } from "solid-js";
import useToastState from "../../../states/toastState";
import { Icon } from "../Icon";
import styles from "./Toasts.module.scss";

function Toasts() {
  const { toasts, dismissToast } = useToastState;

  return (
    <div class={styles.Container} role="status" aria-live="polite">
      <For each={toasts}>
        {(toast) => (
          <div class={`${styles.Toast} ${styles[toast.variant]}`}>
            <span class={styles.Icon}>
              <Switch fallback={<Icon name="info" />}>
                <Match when={toast.variant === "success"}>
                  <Icon name="check" />
                </Match>
                <Match when={toast.variant === "error"}>
                  <Icon name="alert" />
                </Match>
              </Switch>
            </span>
            <span class={styles.Message}>{toast.message}</span>
            <button type="button" class={styles.Close} onClick={() => dismissToast(toast.id)} aria-label="Dismiss">
              <Icon name="x" />
            </button>
          </div>
        )}
      </For>
    </div>
  );
}

export default Toasts;
