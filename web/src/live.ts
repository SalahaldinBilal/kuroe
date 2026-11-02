import { createSignal, onCleanup } from "solid-js";

export const LIVE_SECONDS = 10;

/**
 * Reloads every `seconds` while the tab is visible, and once right away when it becomes visible again.
 * Returns a flag that is true during these background reloads, so views can skip their loading look.
 */
export function useLive(reload: () => unknown, seconds = LIVE_SECONDS): () => boolean {
  const [quiet, setQuiet] = createSignal(false);

  async function tick() {
    if (document.hidden) return;
    setQuiet(true);
    try {
      await reload();
    } catch {
      // A failed reload shows up on the resource itself. The next tick tries again.
    } finally {
      setQuiet(false);
    }
  }

  const timer = setInterval(tick, seconds * 1000);
  function onVisibility() {
    if (!document.hidden) tick();
  }
  document.addEventListener("visibilitychange", onVisibility);
  onCleanup(() => {
    clearInterval(timer);
    document.removeEventListener("visibilitychange", onVisibility);
  });
  return quiet;
}
