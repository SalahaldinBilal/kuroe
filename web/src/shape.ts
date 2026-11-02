import { createEffect } from "solid-js";

const PREFIX = "kuroe.shape.";

/**
 * The shape a view had last time (how many rows, which mode), so its placeholder can match what's about to load.
 * Reads once when the view opens, and saves the new shape whenever `current` is known. Stays in this browser.
 */
export function useShape<T>(key: string, fallback: T, current: () => T | undefined): T {
  let remembered = fallback;
  try {
    const stored = localStorage.getItem(PREFIX + key);
    if (stored != null) remembered = JSON.parse(stored) as T;
  } catch {
    // Storage can be off or full. The fallback shape still works.
  }
  createEffect(() => {
    const now = current();
    if (now === undefined) return;
    try {
      localStorage.setItem(PREFIX + key, JSON.stringify(now));
    } catch {
      // Same as above, nothing to do.
    }
  });
  return remembered;
}

/** Rows to draw for a remembered count, capped so a huge list doesn't draw a huge placeholder. */
export function rowsFor(count: number, max = 10): number {
  return Math.min(Math.max(count, 0), max);
}
