import { createMemo, type JSX, onCleanup, onMount, Show } from "solid-js";
import { Portal } from "solid-js/web";
import Button from "../Button/Button";
import { Icon } from "../Icon";
import styles from "./Modal.module.scss";

export type ModalProps = {
  show: boolean;
  onHide?: () => void;
  title?: JSX.Element;
  subtitle?: JSX.Element;
  footer?: JSX.Element;
  width?: number;
  dismissible?: boolean;
  children: JSX.Element;
};

function Modal(props: ModalProps) {
  const dismissible = createMemo(() => props.dismissible ?? true);

  onMount(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (props.show && dismissible() && event.key === "Escape") props.onHide?.();
    }
    window.addEventListener("keydown", onKeyDown);
    onCleanup(() => window.removeEventListener("keydown", onKeyDown));
  });

  return (
    <Show when={props.show}>
      <Portal>
        <div class={styles.Backdrop}>
          <button
            type="button"
            class={styles.Dismiss}
            tabIndex={-1}
            aria-label="Close"
            onMouseDown={() => dismissible() && props.onHide?.()}
          />
          <div
            class={styles.Dialog}
            role="dialog"
            aria-modal="true"
            style={{ width: props.width ? `${props.width}px` : undefined }}
          >
            <div class={styles.Header}>
              <div class={styles.Titles}>
                <div class={styles.Title}>{props.title}</div>
                <Show when={props.subtitle}>
                  <div class={styles.Subtitle}>{props.subtitle}</div>
                </Show>
              </div>
              <Show when={dismissible()}>
                <Button variant="ghost" icon size="sm" tooltip="Close" onClick={() => props.onHide?.()}>
                  <Icon name="x" />
                </Button>
              </Show>
            </div>
            <div class={styles.Content}>{props.children}</div>
            <Show when={props.footer}>
              <div class={styles.Footer}>{props.footer}</div>
            </Show>
          </div>
        </div>
      </Portal>
    </Show>
  );
}

export default Modal;
