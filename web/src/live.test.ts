import { createRoot } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useLive } from "./live";

function setHidden(hidden: boolean) {
  Object.defineProperty(document, "hidden", { configurable: true, get: () => hidden });
  document.dispatchEvent(new Event("visibilitychange"));
}

describe("useLive", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    setHidden(false);
  });

  it("reloads on the interval, skips hidden tabs, catches up when shown and stops on cleanup", async () => {
    const reload = vi.fn(async () => {});
    const dispose = createRoot((dispose) => {
      useLive(reload, 10);
      return dispose;
    });

    await vi.advanceTimersByTimeAsync(10_000);
    expect(reload).toHaveBeenCalledTimes(1);

    setHidden(true);
    await vi.advanceTimersByTimeAsync(30_000);
    expect(reload).toHaveBeenCalledTimes(1);

    setHidden(false);
    await vi.advanceTimersByTimeAsync(0);
    expect(reload).toHaveBeenCalledTimes(2);

    dispose();
    await vi.advanceTimersByTimeAsync(30_000);
    expect(reload).toHaveBeenCalledTimes(2);
  });

  it("flags the reload as quiet while it runs and keeps going after a failure", async () => {
    let finish: () => void = () => {};
    const reload = vi
      .fn<() => Promise<void>>()
      .mockImplementationOnce(() => new Promise<void>((resolve) => (finish = resolve)))
      .mockRejectedValueOnce(new Error("down"))
      .mockResolvedValue(undefined);
    const quiet = createRoot(() => useLive(reload, 10));

    await vi.advanceTimersByTimeAsync(10_000);
    expect(quiet()).toBe(true);
    finish();
    await vi.advanceTimersByTimeAsync(0);
    expect(quiet()).toBe(false);

    await vi.advanceTimersByTimeAsync(20_000);
    expect(reload).toHaveBeenCalledTimes(3);
    expect(quiet()).toBe(false);
  });
});
