import { createRoot } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { eventsConnected, onEvent, startEvents, stopEvents } from "./eventState";

class FakeSocket {
  static all: FakeSocket[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((message: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;

  constructor(readonly url: string) {
    FakeSocket.all.push(this);
  }

  open() {
    this.onopen?.();
  }

  send(event: object) {
    this.onmessage?.({ data: JSON.stringify(event) });
  }

  close() {
    this.onclose?.();
  }
}

function latest(): FakeSocket {
  return FakeSocket.all[FakeSocket.all.length - 1];
}

describe("eventState", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    FakeSocket.all = [];
    vi.stubGlobal("WebSocket", FakeSocket);
  });
  afterEach(() => {
    stopEvents();
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("reconnects with backoff after a close", async () => {
    startEvents();
    expect(latest().url).toMatch(/\/api\/v1\/events\/ws$/);
    latest().open();
    expect(eventsConnected()).toBe(true);

    latest().close();
    expect(eventsConnected()).toBe(false);
    await vi.advanceTimersByTimeAsync(999);
    expect(FakeSocket.all).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(FakeSocket.all).toHaveLength(2);

    latest().close();
    await vi.advanceTimersByTimeAsync(2000);
    expect(FakeSocket.all).toHaveLength(3);

    latest().open();
    expect(eventsConnected()).toBe(true);
  });

  it("passes matching events and a resync after a reconnect to handlers", async () => {
    const handler = vi.fn();
    const dispose = createRoot((dispose) => {
      onEvent((event) => event.kind === "deployment.status" && event.id === "d1", handler);
      return dispose;
    });
    startEvents();
    latest().open();

    latest().send({ kind: "deployment.status", id: "d2", status: "running", revision: 1 });
    expect(handler).not.toHaveBeenCalled();
    latest().send({ kind: "deployment.status", id: "d1", status: "running", revision: 1 });
    expect(handler).toHaveBeenCalledTimes(1);

    latest().close();
    await vi.advanceTimersByTimeAsync(1000);
    latest().open();
    expect(handler).toHaveBeenCalledTimes(2);

    dispose();
    latest().send({ kind: "deployment.status", id: "d1", status: "failed", revision: 2 });
    expect(handler).toHaveBeenCalledTimes(2);
  });

  it("debounces bursts and stops reconnecting once stopped", async () => {
    const handler = vi.fn();
    createRoot(() => onEvent((event) => event.kind === "container.state", handler, 500));
    startEvents();
    latest().open();
    for (let i = 0; i < 5; i++)
      latest().send({ kind: "container.state", deployment: "d1", container: "c", state: "exited" });
    await vi.advanceTimersByTimeAsync(500);
    expect(handler).toHaveBeenCalledTimes(1);

    stopEvents();
    await vi.advanceTimersByTimeAsync(20_000);
    expect(FakeSocket.all).toHaveLength(1);
  });
});
