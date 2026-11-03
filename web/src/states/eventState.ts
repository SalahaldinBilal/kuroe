import { createSignal, onCleanup } from "solid-js";
import { type DeployStepName, socketUrl } from "../api";

export type LiveEvent =
  | { kind: "deployment.status"; id: string; status: string; revision: number }
  | { kind: "deployment.changed"; id: string }
  | { kind: "deploy.step"; id: string; step: DeployStepName; detail: string | null }
  | { kind: "container.state"; deployment: string; container: string; state: string }
  | { kind: "proxy.traefik"; running: boolean }
  | { kind: "runner.instance"; pool: string; container: string; state: string }
  | { kind: "resync" };

type Handler = (event: LiveEvent) => void;

export const RETRY_SECONDS = [1, 2, 5, 10];
export const EVENT_DEBOUNCE_MS = 500;

export function isDeploymentEvent(event: LiveEvent): boolean {
  return event.kind === "deployment.status" || event.kind === "deployment.changed" || event.kind === "container.state";
}

const handlers = new Set<Handler>();
const [connected, setConnected] = createSignal(false);
let socket: WebSocket | null = null;
let attempt = 0;
let retry: ReturnType<typeof setTimeout> | undefined;
let wanted = false;

function dispatch(event: LiveEvent) {
  for (const handler of handlers) handler(event);
}

function parse(text: unknown): LiveEvent | null {
  try {
    return JSON.parse(String(text)) as LiveEvent;
  } catch {
    return null;
  }
}

function open() {
  const current = new WebSocket(socketUrl("/events/ws"));
  socket = current;
  current.onopen = () => {
    if (attempt > 0) dispatch({ kind: "resync" });
    attempt = 0;
    setConnected(true);
  };
  current.onmessage = (message) => {
    const event = parse(message.data);
    if (event) dispatch(event);
  };
  current.onclose = () => {
    if (socket !== current) return;
    socket = null;
    setConnected(false);
    if (!wanted) return;
    const delay = RETRY_SECONDS[Math.min(attempt, RETRY_SECONDS.length - 1)];
    attempt++;
    retry = setTimeout(open, delay * 1000);
  };
}

export function startEvents() {
  if (wanted) return;
  wanted = true;
  open();
}

export function stopEvents() {
  wanted = false;
  clearTimeout(retry);
  const current = socket;
  socket = null;
  attempt = 0;
  setConnected(false);
  current?.close();
}

export const eventsConnected = connected;

/** Calls `listener` with every event while the calling component lives, for pages that use what the event says. */
export function onEachEvent(listener: (event: LiveEvent) => void) {
  handlers.add(listener);
  onCleanup(() => handlers.delete(listener));
}

/** Calls `handler` for matching events while the calling component lives. A resync always matches. */
export function onEvent(matches: (event: LiveEvent) => boolean, handler: () => void, debounceMs = 0) {
  let timer: ReturnType<typeof setTimeout> | undefined;
  function listener(event: LiveEvent) {
    if (event.kind !== "resync" && !matches(event)) return;
    if (debounceMs === 0) return handler();
    clearTimeout(timer);
    timer = setTimeout(handler, debounceMs);
  }
  handlers.add(listener);
  onCleanup(() => {
    handlers.delete(listener);
    clearTimeout(timer);
  });
}
