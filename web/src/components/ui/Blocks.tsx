import type { JSX } from "solid-js";
import { For, Show } from "solid-js";
import { Icon, type IconName } from "./Icon";
import { tooltip } from "./Tooltip/Tooltip";

export function PageHeader(props: { title: JSX.Element; description?: JSX.Element; actions?: JSX.Element }) {
  return (
    <div class="ph">
      <div>
        <h1>{props.title}</h1>
        <Show when={props.description}>
          <div class="sub">{props.description}</div>
        </Show>
      </div>
      <Show when={props.actions}>
        <div class="acts">{props.actions}</div>
      </Show>
    </div>
  );
}

interface CardProps {
  title?: JSX.Element;
  description?: JSX.Element;
  actions?: JSX.Element;
  class?: string;
  children?: JSX.Element;
}

export function Card(props: CardProps) {
  return (
    <section class={props.class ? `card ${props.class}` : "card"}>
      <Show when={props.title}>
        <header class="card-h">
          <div>
            <h2>{props.title}</h2>
            <Show when={props.description}>
              <div class="d">{props.description}</div>
            </Show>
          </div>
          <Show when={props.actions}>
            <div class="acts">{props.actions}</div>
          </Show>
        </header>
      </Show>
      {props.children}
    </section>
  );
}

export function Note(props: {
  tone?: "warn" | "bad" | "ok";
  icon?: IconName;
  children: JSX.Element;
  action?: JSX.Element;
}) {
  const icon = (): IconName => props.icon ?? (props.tone === "ok" ? "check" : props.tone ? "alert" : "info");
  return (
    <div class={props.tone ? `note ${props.tone}` : "note"}>
      <Icon name={icon()} />
      <span class="grow">{props.children}</span>
      {props.action}
    </div>
  );
}

export function ExtLink(props: { href: string; class?: string; children: JSX.Element }) {
  return (
    <a class={props.class ? `link ext ${props.class}` : "link ext"} href={props.href} target="_blank" rel="noreferrer">
      {props.children}
      <Icon name="ext" />
    </a>
  );
}

export function StatusBadge(props: { status: string }) {
  return (
    <span class={`badge s-${props.status}`}>
      <i />
      {props.status}
    </span>
  );
}

export function Pill(props: { tone?: "ok" | "warn" | "bad"; dot?: boolean; tooltip?: string; children: JSX.Element }) {
  return (
    <span
      ref={(element) => tooltip(element, () => props.tooltip)}
      class={props.tone ? `badge s-${props.tone}` : "badge"}
    >
      <Show when={props.dot}>
        <i />
      </Show>
      {props.children}
    </span>
  );
}

export function Steps(props: { labels: string[]; step: number }) {
  return (
    <div class="steps">
      <For each={props.labels}>
        {(label, i) => (
          <>
            <Show when={i() > 0}>
              <span class="ln" />
            </Show>
            <span class={props.step === i() ? "step on" : props.step > i() ? "step done" : "step"}>
              <span class="n">
                <Show when={props.step > i()} fallback={i() + 1}>
                  <Icon name="check" />
                </Show>
              </span>
              {label}
            </span>
          </>
        )}
      </For>
    </div>
  );
}

export function Chevron() {
  return <Icon name="right" class="chev" />;
}
