import { createEffect, createSignal, For, Match, on, onCleanup, Show, Switch } from "solid-js";
import { api, type DeployStep, type LiveDeployMessage } from "../api";
import { STEP_LABELS, stepSeconds } from "../deploys";
import { formatDuration } from "../format";
import { onEvent, RETRY_SECONDS } from "../states/eventState";
import { AnsiText } from "./AnsiText";
import { Icon } from "./ui/Icon";
import Spinner from "./ui/Spinner/Spinner";

/** Close reasons the server sends when the stream is over. Anything else is a dropped connection. */
const ENDED = new Set(["finished", "idle"]);

export function StepList(props: { steps: DeployStep[] }) {
  const [now, setNow] = createSignal(Date.now());
  createEffect(() => {
    if (!props.steps.some((s) => s.started_ms != null && s.finished_ms == null)) return;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    onCleanup(() => clearInterval(timer));
  });

  function state(step: DeployStep) {
    if (step.started_ms == null) return "wait";
    if (step.finished_ms == null) return "on";
    return step.failed ? "bad" : "done";
  }

  return (
    <ol class="deploy-steps">
      <For each={props.steps}>
        {(step) => (
          <li class={state(step)}>
            <span class="mark">
              <Switch>
                <Match when={state(step) === "on"}>
                  <Spinner size={14} />
                </Match>
                <Match when={state(step) === "wait"}>
                  <span class="pip" />
                </Match>
                <Match when={state(step) === "bad"}>
                  <Icon name="x" />
                </Match>
                <Match when={state(step) === "done"}>
                  <Icon name="check" />
                </Match>
              </Switch>
            </span>
            <span class="label">{STEP_LABELS[step.step]}</span>
            <Show when={step.detail}>
              <span class="detail">{step.detail}</span>
            </Show>
            <Show when={state(step) !== "wait"}>
              <span class="time">{formatDuration(stepSeconds(step, now()))}</span>
            </Show>
          </li>
        )}
      </For>
    </ol>
  );
}

/** With `follow`, stays at the bottom as lines come in, unless the reader scrolled up. */
export function DeployOutput(props: { text: string; follow?: boolean; height?: string }) {
  let pre: HTMLPreElement | undefined;
  let stick = true;

  createEffect(
    on(
      () => props.text,
      () => {
        if (pre && props.follow && stick) pre.scrollTop = pre.scrollHeight;
      },
    ),
  );

  function onScroll() {
    if (pre) stick = pre.scrollTop + pre.clientHeight >= pre.scrollHeight - 24;
  }

  return (
    <pre class="term deploy-out" ref={pre} style={{ "max-height": props.height ?? "360px" }} onScroll={onScroll}>
      <AnsiText text={props.text} />
    </pre>
  );
}

function parse(data: unknown): LiveDeployMessage | null {
  try {
    return JSON.parse(String(data)) as LiveDeployMessage;
  } catch {
    return null;
  }
}

/** Follows the running deploy. `onEnd` runs once the server says the stream is over. */
export function LiveDeploy(props: { deployment: string; onEnd: () => void }) {
  const [text, setText] = createSignal("");
  const [steps, setSteps] = createSignal<DeployStep[]>([]);
  let lines: string[] = [];
  let partial: string | null = null;
  let frame: number | undefined;
  let socket: WebSocket | null = null;
  let retry: ReturnType<typeof setTimeout> | undefined;
  let attempt = 0;
  let disposed = false;

  function show() {
    if (frame !== undefined) return;
    frame = requestAnimationFrame(() => {
      frame = undefined;
      setText(partial == null ? lines.join("\n") : [...lines, partial].join("\n"));
    });
  }

  function receive(message: LiveDeployMessage) {
    if (message.kind === "steps") {
      setSteps(message.steps);
      return;
    }
    if (message.kind === "snapshot") {
      lines = [...message.lines];
      partial = message.partial;
      setSteps(message.steps);
    } else if (message.kind === "line") {
      lines.push(message.text);
      partial = null;
    } else {
      partial = message.text;
    }
    show();
  }

  function open() {
    clearTimeout(retry);
    const previous = socket;
    socket = null;
    previous?.close();
    const current = new WebSocket(api.deploySocket(props.deployment));
    socket = current;
    current.onopen = () => {
      attempt = 0;
    };
    current.onmessage = (event) => {
      const message = parse(event.data);
      if (message) receive(message);
    };
    current.onclose = (event) => {
      if (socket !== current || disposed) return;
      socket = null;
      if (ENDED.has(event.reason)) {
        props.onEnd();
        return;
      }
      const delay = RETRY_SECONDS[Math.min(attempt, RETRY_SECONDS.length - 1)];
      attempt++;
      retry = setTimeout(open, delay * 1000);
    };
  }

  open();
  onEvent(
    () => false,
    () => {
      open();
      props.onEnd();
    },
  );
  onCleanup(() => {
    disposed = true;
    clearTimeout(retry);
    if (frame !== undefined) cancelAnimationFrame(frame);
    const current = socket;
    socket = null;
    current?.close();
  });

  return (
    <>
      <Show when={steps().length > 0}>
        <StepList steps={steps()} />
      </Show>
      <Show when={text()}>
        <DeployOutput text={text()} follow />
      </Show>
    </>
  );
}
