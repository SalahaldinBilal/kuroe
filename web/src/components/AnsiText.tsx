import { createMemo, For, type JSX } from "solid-js";
import { type AnsiLine, type AnsiStyle, parseAnsi } from "../ansi";

function css(style: AnsiStyle): JSX.CSSProperties | undefined {
  const fg = style.inverse ? (style.bg ?? "var(--term-bg)") : style.fg;
  const bg = style.inverse ? (style.fg ?? "var(--term-text)") : style.bg;
  if (!fg && !bg && !style.bold && !style.dim && !style.italic && !style.underline) return undefined;
  return {
    color: fg,
    "background-color": bg,
    "font-weight": style.bold ? "600" : undefined,
    opacity: style.dim ? "0.7" : undefined,
    "font-style": style.italic ? "italic" : undefined,
    "text-decoration": style.underline ? "underline" : undefined,
  };
}

export function AnsiText(props: { text: string }) {
  const cache = new Map();
  const lines = createMemo<AnsiLine[]>(() => parseAnsi(props.text, cache));
  return (
    <For each={lines()}>
      {(line, i) => (
        <>
          {i() > 0 ? "\n" : ""}
          <For each={line}>
            {(segment) => {
              const style = css(segment.style);
              return style ? <span style={style}>{segment.text}</span> : segment.text;
            }}
          </For>
        </>
      )}
    </For>
  );
}
