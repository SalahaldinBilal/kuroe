import { createEffect, createSignal, For, onCleanup, Show } from "solid-js";
import { Portal } from "solid-js/web";
import type { TimeWindow } from "../api";
import styles from "./RangePicker.module.scss";
import Button from "./ui/Button/Button";
import Field from "./ui/Field/Field";
import { Icon } from "./ui/Icon";
import Input from "./ui/Input/Input";
import Spinner from "./ui/Spinner/Spinner";

export type Preset = "1h" | "6h" | "24h" | "7d" | "30d";

/** A preset follows the current time. A custom window stays where the user put it. */
export type Range = { preset: Preset } | TimeWindow;

const PRESETS: { value: Preset; label: string; seconds: number }[] = [
  { value: "1h", label: "Last hour", seconds: 3_600 },
  { value: "6h", label: "Last 6 hours", seconds: 6 * 3_600 },
  { value: "24h", label: "Last 24 hours", seconds: 86_400 },
  { value: "7d", label: "Last 7 days", seconds: 7 * 86_400 },
  { value: "30d", label: "Last 30 days", seconds: 30 * 86_400 },
];

const DAY = 86_400;
const MIN_SPAN = 60;
const MAX_SPAN = 400 * DAY;
const WEEKDAYS = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const MONTH_NAMES = [
  "January",
  "February",
  "March",
  "April",
  "May",
  "June",
  "July",
  "August",
  "September",
  "October",
  "November",
  "December",
];
const EDGE = 16;

export const isLive = (range: Range): boolean => "preset" in range;

export function windowOf(range: Range, now = Date.now() / 1000): TimeWindow {
  if ("preset" in range) {
    const to = Math.floor(now);
    const seconds = PRESETS.find((p) => p.value === range.preset)?.seconds ?? 3_600;
    return { from: to - seconds, to };
  }
  return { from: range.from, to: range.to };
}

export function rangeDomain(range: Range): [number, number] {
  const w = windowOf(range);
  return [w.from, w.to];
}

/** `2026-11-02 08:00`, in UTC like the charts. */
export function fieldText(t: number): string {
  return new Date(t * 1000).toISOString().slice(0, 16).replace("T", " ");
}

export function parseField(text: string): number | null {
  const match = text.trim().match(/^(\d{4}-\d{2}-\d{2})[ T](\d{1,2}):(\d{2})(?::(\d{2}))?$/);
  if (!match) return null;
  const [, date, hours, minutes, seconds] = match;
  if (Number(hours) > 23) return null;
  const ms = Date.parse(`${date}T${hours.padStart(2, "0")}:${minutes}:${seconds ?? "00"}Z`);
  return Number.isNaN(ms) ? null : Math.floor(ms / 1000);
}

/** The window two fields describe, or the reason it isn't one. */
export function checkWindow(fromText: string, toText: string): TimeWindow | string {
  const from = parseField(fromText);
  const to = parseField(toText);
  if (from == null || to == null) return "Write times like 2026-11-02 08:00.";
  if (to - from < MIN_SPAN) return "The end has to be at least a minute after the start.";
  if (to - from > MAX_SPAN) return "Pick at most 400 days.";
  return { from, to };
}

function dayOf(t: number): number {
  return Math.floor(t / DAY) * DAY;
}

function shortDate(t: number, now: number): string {
  const d = new Date(t * 1000);
  const year = d.getUTCFullYear() === new Date(now * 1000).getUTCFullYear() ? "" : ` ${d.getUTCFullYear()}`;
  return `${MONTHS[d.getUTCMonth()]} ${d.getUTCDate()}${year}`;
}

function clock(t: number): string {
  return new Date(t * 1000).toISOString().slice(11, 16);
}

export function rangeLabel(range: Range, now = Date.now() / 1000): string {
  if ("preset" in range) return PRESETS.find((p) => p.value === range.preset)?.label ?? "";
  const from = `${shortDate(range.from, now)}, ${clock(range.from)}`;
  const to =
    dayOf(range.from) === dayOf(range.to) ? clock(range.to) : `${shortDate(range.to, now)}, ${clock(range.to)}`;
  return `${from} to ${to} UTC`;
}

/** Six weeks starting on the Monday on or before the first of the month, as UTC midnights. */
export function monthCells(year: number, month: number): number[] {
  const first = Date.UTC(year, month, 1) / 1000;
  const weekday = (new Date(first * 1000).getUTCDay() + 6) % 7;
  const start = first - weekday * DAY;
  return Array.from({ length: 42 }, (_, i) => start + i * DAY);
}

/** Moves one end's date to `day` and keeps its time. A first click picks the start, a second the end. */
export function pickDay(
  fromText: string,
  toText: string,
  day: number,
  picking: "start" | "end",
): { from: string; to: string; picking: "start" | "end" } {
  const from = parseField(fromText);
  const to = parseField(toText);
  const fromTime = from == null ? 0 : from - dayOf(from);
  const toTime = to == null ? DAY - 60 : to - dayOf(to);
  if (picking === "start" || from == null) {
    return { from: fieldText(day + fromTime), to: fieldText(day + toTime), picking: "end" };
  }
  if (day < dayOf(from)) {
    return { from: fieldText(day + fromTime), to: fieldText(dayOf(from) + toTime), picking: "start" };
  }
  return { from: fromText, to: fieldText(day + toTime), picking: "start" };
}

/** Stays up a moment after a fast reload, so it reads as a status instead of a flicker. */
const REFRESH_SHOWN_MS = 700;

export function LiveStatus(props: { refreshing: boolean }) {
  const [shown, setShown] = createSignal(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  createEffect(() => {
    clearTimeout(timer);
    if (props.refreshing) setShown(true);
    else timer = setTimeout(() => setShown(false), REFRESH_SHOWN_MS);
  });
  onCleanup(() => clearTimeout(timer));
  return (
    <span class="live-status" aria-live="polite">
      <Show
        when={shown()}
        fallback={
          <>
            <span class="live-dot" />
            Live
          </>
        }
      >
        <Spinner size={12} />
        Refreshing
      </Show>
    </span>
  );
}

export function RangePicker(props: { value: Range; onChange: (range: Range) => void }) {
  const [open, setOpen] = createSignal(false);
  const [fromText, setFromText] = createSignal("");
  const [toText, setToText] = createSignal("");
  const [picking, setPicking] = createSignal<"start" | "end">("start");
  const [view, setView] = createSignal({ year: 2026, month: 0 });
  const [position, setPosition] = createSignal({ top: 0, left: 0 });
  let trigger: HTMLElement | undefined;
  let popover: HTMLDivElement | undefined;

  const checked = () => checkWindow(fromText(), toText());
  const valid = () => typeof checked() !== "string";
  const today = () => dayOf(Date.now() / 1000);
  const span = () => {
    const from = parseField(fromText());
    const to = parseField(toText());
    return from == null || to == null ? null : { from: dayOf(from), to: dayOf(to) };
  };

  function show() {
    const w = windowOf(props.value);
    setFromText(fieldText(w.from));
    setToText(fieldText(w.to));
    setPicking("start");
    const end = new Date(w.to * 1000);
    setView({ year: end.getUTCFullYear(), month: end.getUTCMonth() });
    setOpen(true);
  }

  function place() {
    if (!trigger || !popover) return;
    const anchor = trigger.getBoundingClientRect();
    const width = popover.offsetWidth;
    const height = popover.offsetHeight;
    let left = anchor.right - width;
    left = Math.min(Math.max(EDGE, left), window.innerWidth - width - EDGE);
    let top = anchor.bottom + 6;
    if (top + height > window.innerHeight - EDGE && anchor.top - height - 6 > EDGE) top = anchor.top - height - 6;
    setPosition({ top, left: Math.max(EDGE, left) });
  }

  createEffect(() => {
    if (!open()) return;
    queueMicrotask(place);
    function onKey(event: KeyboardEvent) {
      if (event.key === "Escape") setOpen(false);
    }
    function onPointer(event: PointerEvent) {
      const target = event.target as Node;
      if (!popover?.contains(target) && !trigger?.contains(target)) setOpen(false);
    }
    window.addEventListener("keydown", onKey);
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    document.addEventListener("pointerdown", onPointer, true);
    onCleanup(() => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("resize", place);
      window.removeEventListener("scroll", place, true);
      document.removeEventListener("pointerdown", onPointer, true);
    });
  });

  function choosePreset(preset: Preset) {
    props.onChange({ preset });
    setOpen(false);
  }

  function choose(day: number) {
    const next = pickDay(fromText(), toText(), day, picking());
    setFromText(next.from);
    setToText(next.to);
    setPicking(next.picking);
  }

  function shiftMonth(by: number) {
    const { year, month } = view();
    const d = new Date(Date.UTC(year, month + by, 1));
    setView({ year: d.getUTCFullYear(), month: d.getUTCMonth() });
  }

  function apply() {
    const w = checked();
    if (typeof w === "string") return;
    props.onChange(w);
    setOpen(false);
  }

  const activePreset = () => ("preset" in props.value ? props.value.preset : null);

  return (
    <>
      <Button
        ref={(el) => {
          trigger = el;
        }}
        class={styles.Trigger}
        active={open()}
        onClick={() => (open() ? setOpen(false) : show())}
      >
        <Icon name="calendar" />
        <span class={styles.Label}>{rangeLabel(props.value)}</span>
        <Icon name="down" />
      </Button>
      <Show when={open()}>
        <Portal>
          <div
            ref={popover}
            class={styles.Popover}
            role="dialog"
            aria-label="Time range"
            style={{ top: `${position().top}px`, left: `${position().left}px` }}
          >
            <div class={styles.Presets}>
              <For each={PRESETS}>
                {(p) => (
                  <Button
                    variant="ghost"
                    size="sm"
                    block
                    class={styles.Preset}
                    active={activePreset() === p.value}
                    onClick={() => choosePreset(p.value)}
                  >
                    <span class={styles.Label}>{p.label}</span>
                    <Show when={activePreset() === p.value}>
                      <Icon name="check" />
                    </Show>
                  </Button>
                )}
              </For>
            </div>
            <div class={styles.Custom}>
              <div class={styles.MonthHead}>
                <Button variant="ghost" size="sm" icon tooltip="Previous month" onClick={() => shiftMonth(-1)}>
                  <Icon name="left" />
                </Button>
                <span>
                  {MONTH_NAMES[view().month]} {view().year}
                </span>
                <Button variant="ghost" size="sm" icon tooltip="Next month" onClick={() => shiftMonth(1)}>
                  <Icon name="right" />
                </Button>
              </div>
              <div class={styles.Grid}>
                <For each={WEEKDAYS}>{(d) => <span class={styles.Weekday}>{d}</span>}</For>
                <For each={monthCells(view().year, view().month)}>
                  {(day) => {
                    const date = new Date(day * 1000);
                    const s = () => span();
                    const inSpan = () => {
                      const r = s();
                      return !!r && day >= r.from && day <= r.to;
                    };
                    const edge = () => {
                      const r = s();
                      return !!r && (day === r.from || day === r.to);
                    };
                    return (
                      <button
                        type="button"
                        class={styles.Day}
                        classList={{
                          [styles.Outside]: date.getUTCMonth() !== view().month,
                          [styles.InSpan]: inSpan(),
                          [styles.Edge]: edge(),
                          [styles.Today]: day === today(),
                        }}
                        disabled={day > today()}
                        aria-label={fieldText(day).slice(0, 10)}
                        onClick={() => choose(day)}
                      >
                        {date.getUTCDate()}
                      </button>
                    );
                  }}
                </For>
              </div>
              <div class={styles.Fields}>
                <Field label="From">
                  <Input
                    mono
                    label="From, UTC"
                    placeholder="2026-11-02 08:00"
                    value={fromText()}
                    onValue={setFromText}
                  />
                </Field>
                <Field label="To">
                  <Input mono label="To, UTC" placeholder="2026-11-02 12:00" value={toText()} onValue={setToText} />
                </Field>
              </div>
              <div class={styles.Footer}>
                <span class={styles.Note} classList={{ [styles.Bad]: !valid() }}>
                  {valid() ? "Times are UTC." : (checked() as string)}
                </span>
                <Button variant="ghost" size="sm" onClick={() => setOpen(false)}>
                  Cancel
                </Button>
                <Button variant="primary" size="sm" disabled={!valid()} onClick={apply}>
                  Apply
                </Button>
              </div>
            </div>
          </div>
        </Portal>
      </Show>
    </>
  );
}
