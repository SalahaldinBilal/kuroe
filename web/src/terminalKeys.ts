type Key = Pick<KeyboardEvent, "type" | "key" | "ctrlKey" | "metaKey" | "shiftKey" | "altKey">;

/** Ctrl+C (Cmd+C on a Mac) copies a selection, and Ctrl+Shift+C always copies, like most terminal apps. */
export function copyKey(event: Key, hasSelection: boolean): "copy" | "swallow" | "pass" {
  if (event.type !== "keydown" || event.altKey || event.key.toLowerCase() !== "c") return "pass";
  if (!event.ctrlKey && !event.metaKey) return "pass";
  if (event.shiftKey) return hasSelection ? "copy" : "swallow";
  return hasSelection ? "copy" : "pass";
}
