import { Show } from "solid-js";
import styles from "./ProgressBar.module.scss";

function ProgressBar(props: { value?: number; class?: string }) {
  return (
    <div
      class={`${styles.ProgressBar} ${props.class ?? ""}`}
      role="progressbar"
      aria-valuenow={props.value}
      aria-valuemin={0}
      aria-valuemax={100}
    >
      <Show
        when={props.value !== undefined}
        fallback={
          <>
            <span class={styles.LeadBar} />
            <span class={styles.TrailBar} />
          </>
        }
      >
        <span class={styles.ValueBar} style={{ transform: `translateX(${(props.value ?? 0) - 100}%)` }} />
      </Show>
    </div>
  );
}

export default ProgressBar;
