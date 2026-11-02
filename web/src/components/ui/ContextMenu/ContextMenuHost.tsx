import { createEffect, createSignal, For, onCleanup, Show } from "solid-js";
import { Portal } from "solid-js/web";
import useContextMenuState, { isSeparator, type MenuAction, type OpenMenu } from "../../../states/contextMenuState";
import { Icon } from "../Icon";
import styles from "./ContextMenu.module.scss";

const EDGE_MARGIN = 4;

function MenuList(props: { menu: OpenMenu }) {
  const { closeContextMenu } = useContextMenuState;
  const [position, setPosition] = createSignal(props.menu.position);

  function place(element: HTMLElement) {
    queueMicrotask(() => {
      const { offsetWidth: width, offsetHeight: height } = element;
      let x = props.menu.alignRight ? props.menu.position.x - width : props.menu.position.x;
      if (x + width > window.innerWidth - EDGE_MARGIN) x = window.innerWidth - width - EDGE_MARGIN;
      const y = Math.min(props.menu.position.y, window.innerHeight - height - EDGE_MARGIN);
      setPosition({ x: Math.max(EDGE_MARGIN, x), y: Math.max(EDGE_MARGIN, y) });
    });
  }

  return (
    <div ref={place} class={styles.Menu} style={{ left: `${position().x}px`, top: `${position().y}px` }} role="menu">
      <For each={props.menu.items}>
        {(item) => (
          <Show when={!isSeparator(item) && (item as MenuAction)} fallback={<div class={styles.Separator} />}>
            {(entry) => (
              <button
                type="button"
                class={styles.Item}
                classList={{ [styles.Danger]: entry().danger ?? false }}
                role="menuitem"
                disabled={entry().disabled}
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => {
                  closeContextMenu();
                  entry().onSelect?.();
                }}
              >
                <span class={styles.Icon}>
                  <Show when={entry().icon}>{(icon) => <Icon name={icon()} />}</Show>
                </span>
                <span class={styles.Label}>{entry().label}</span>
                <Show when={entry().hint}>
                  <span class={styles.Hint}>{entry().hint}</span>
                </Show>
              </button>
            )}
          </Show>
        )}
      </For>
    </div>
  );
}

function ContextMenuHost() {
  const { menu, closeContextMenu } = useContextMenuState;
  let root: HTMLDivElement | undefined;

  createEffect(() => {
    if (!menu()) return;

    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") closeContextMenu();
    }
    function onPointerDown(event: PointerEvent) {
      if (!root?.contains(event.target as Node)) closeContextMenu();
    }

    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("blur", closeContextMenu);
    window.addEventListener("resize", closeContextMenu);
    window.addEventListener("scroll", closeContextMenu, true);
    // Registered a frame later so the click that opened the menu doesn't close it.
    const frame = requestAnimationFrame(() => document.addEventListener("pointerdown", onPointerDown, true));
    onCleanup(() => {
      cancelAnimationFrame(frame);
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("blur", closeContextMenu);
      window.removeEventListener("resize", closeContextMenu);
      window.removeEventListener("scroll", closeContextMenu, true);
      document.removeEventListener("pointerdown", onPointerDown, true);
    });
  });

  return (
    <Show when={menu()} keyed>
      {(current) => (
        <Portal>
          <div ref={root} class={styles.Root}>
            <MenuList menu={current} />
          </div>
        </Portal>
      )}
    </Show>
  );
}

export default ContextMenuHost;
