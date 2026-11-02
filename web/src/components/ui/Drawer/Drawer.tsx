import { createEffect, createSignal, type JSX, onCleanup, onMount, Show, untrack } from "solid-js";
import { Portal } from "solid-js/web";
import Button from "../Button/Button";
import { Icon } from "../Icon";
import styles from "./Drawer.module.scss";

export interface DrawerProps {
  show: boolean;
  onClose?: () => void;
  width?: number;
  header?: JSX.Element;
  children?: JSX.Element;
}

const DEFAULT_WIDTH = 820;
const DURATION = 225;

function Drawer(props: DrawerProps) {
  const width = () => Math.min(props.width ?? DEFAULT_WIDTH, window.innerWidth);
  const [rendered, setRendered] = createSignal(props.show);
  const [current, setCurrent] = createSignal(props.show ? 1 : 0);
  const [duration, setDuration] = createSignal(0);
  let exitTimer: number | undefined;

  createEffect(() => {
    if (props.show) {
      clearTimeout(exitTimer);
      if (untrack(rendered)) {
        setDuration(DURATION);
        setCurrent(1);
      } else {
        // Mount closed, then animate open on the next frame so CSS has a start state.
        setDuration(0);
        setCurrent(0);
        setRendered(true);
        requestAnimationFrame(() =>
          requestAnimationFrame(() => {
            setDuration(DURATION);
            setCurrent(1);
          }),
        );
      }
    } else if (untrack(rendered)) {
      setDuration(DURATION);
      setCurrent(0);
      exitTimer = window.setTimeout(() => setRendered(false), DURATION);
    }
  });

  onMount(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (props.show && event.key === "Escape") props.onClose?.();
    }
    window.addEventListener("keydown", onKeyDown);
    onCleanup(() => window.removeEventListener("keydown", onKeyDown));
  });

  onCleanup(() => clearTimeout(exitTimer));

  const transition = () =>
    `transform ${duration()}ms cubic-bezier(0.4, 0, 0.2, 1), opacity ${duration()}ms cubic-bezier(0.4, 0, 0.2, 1)`;

  return (
    <Show when={rendered()}>
      <Portal>
        <div class={styles.Root}>
          <button
            type="button"
            class={styles.Backdrop}
            tabIndex={-1}
            aria-label="Close panel"
            style={{ opacity: current(), transition: transition() }}
            onClick={() => props.onClose?.()}
          />
          <aside
            class={styles.Panel}
            role="dialog"
            aria-modal="true"
            style={{
              width: `${width()}px`,
              transform: `translateX(${(1 - current()) * width()}px)`,
              transition: transition(),
            }}
          >
            <div class={styles.Header}>
              {props.header}
              <Button variant="ghost" icon tooltip="Close" onClick={() => props.onClose?.()}>
                <Icon name="x" />
              </Button>
            </div>
            <div class={styles.Body}>{props.children}</div>
          </aside>
        </div>
      </Portal>
    </Show>
  );
}

export default Drawer;
