import { createRoot, createSignal } from "solid-js";
import type { IconName } from "../components/ui/Icon";

export type Position = { x: number; y: number };

export type MenuAction = {
  label: string;
  icon?: IconName;
  hint?: string;
  danger?: boolean;
  disabled?: boolean;
  onSelect?: () => void;
};

export type MenuItem = MenuAction | "separator";

export type OpenMenu = { items: MenuItem[]; position: Position; alignRight: boolean };

export const isSeparator = (item: MenuItem): item is "separator" => item === "separator";

function useContextMenuStateInner() {
  const [menu, setMenu] = createSignal<OpenMenu | null>(null);

  function openContextMenu(items: MenuItem[], position: Position) {
    setMenu({ items, position, alignRight: false });
  }

  // For a "more" button: the menu's right edge lines up with the button's.
  function openMenuBelow(element: HTMLElement, items: MenuItem[]) {
    const rect = element.getBoundingClientRect();
    setMenu({ items, position: { x: rect.right, y: rect.bottom + 4 }, alignRight: true });
  }

  function closeContextMenu() {
    setMenu(null);
  }

  return { menu, openContextMenu, openMenuBelow, closeContextMenu };
}

const useContextMenuState = createRoot(useContextMenuStateInner);
export default useContextMenuState;
