export interface AnsiStyle {
  fg?: string;
  bg?: string;
  bold?: boolean;
  dim?: boolean;
  italic?: boolean;
  underline?: boolean;
  inverse?: boolean;
}

export interface Segment {
  text: string;
  style: AnsiStyle;
}

export type AnsiLine = Segment[];

type Parsed = { segments: AnsiLine; end: AnsiStyle };

const ESC = "\u001b";
const CUBE = [0, 95, 135, 175, 215, 255];

export function color256(n: number): string | undefined {
  if (!Number.isInteger(n) || n < 0 || n > 255) return undefined;
  if (n < 16) return `var(--ansi-${n})`;
  if (n < 232) {
    const i = n - 16;
    return `rgb(${CUBE[Math.floor(i / 36)]} ${CUBE[Math.floor(i / 6) % 6]} ${CUBE[i % 6]})`;
  }
  const grey = 8 + (n - 232) * 10;
  return `rgb(${grey} ${grey} ${grey})`;
}

function extended(codes: number[], i: number): { color: string | undefined; used: number } {
  if (codes[i + 1] === 5) return { color: color256(codes[i + 2]), used: 2 };
  if (codes[i + 1] === 2) {
    const [r, g, b] = codes.slice(i + 2, i + 5);
    const ok = [r, g, b].every((v) => Number.isInteger(v) && v >= 0 && v <= 255);
    return { color: ok ? `rgb(${r} ${g} ${b})` : undefined, used: 4 };
  }
  return { color: undefined, used: 0 };
}

export function applySgr(style: AnsiStyle, params: string): AnsiStyle {
  const codes = params === "" ? [0] : params.split(/[;:]/).map((p) => (p === "" ? 0 : Number(p)));
  let next = { ...style };
  for (let i = 0; i < codes.length; i++) {
    const c = codes[i];
    if (c === 0) next = {};
    else if (c === 1) next.bold = true;
    else if (c === 2) next.dim = true;
    else if (c === 3) next.italic = true;
    else if (c === 4) next.underline = true;
    else if (c === 7) next.inverse = true;
    else if (c === 22) next.bold = next.dim = false;
    else if (c === 23) next.italic = false;
    else if (c === 24) next.underline = false;
    else if (c === 27) next.inverse = false;
    else if (c >= 30 && c <= 37) next.fg = color256(c - 30);
    else if (c >= 90 && c <= 97) next.fg = color256(c - 90 + 8);
    else if (c === 39) next.fg = undefined;
    else if (c >= 40 && c <= 47) next.bg = color256(c - 40);
    else if (c >= 100 && c <= 107) next.bg = color256(c - 100 + 8);
    else if (c === 49) next.bg = undefined;
    else if (c === 38 || c === 48) {
      const { color, used } = extended(codes, i);
      if (c === 38) next.fg = color;
      else next.bg = color;
      i += used;
    }
  }
  return next;
}

function scan(line: string, start: AnsiStyle): Parsed {
  const segments: AnsiLine = [];
  let style = start;
  let text = "";
  function flush() {
    if (!text) return;
    const last = segments[segments.length - 1];
    if (last && last.style === style) last.text += text;
    else segments.push({ text, style });
    text = "";
  }
  let i = 0;
  while (i < line.length) {
    if (line[i] !== ESC) {
      text += line[i];
      i++;
      continue;
    }
    const kind = line[i + 1];
    if (kind === "[") {
      let j = i + 2;
      while (j < line.length && !(line[j] >= "@" && line[j] <= "~")) j++;
      if (line[j] === "m") {
        flush();
        style = applySgr(style, line.slice(i + 2, j));
      }
      i = j + 1;
    } else if (kind === "]") {
      let j = i + 2;
      while (j < line.length && line[j] !== "\u0007" && !(line[j] === ESC && line[j + 1] === "\\")) j++;
      i = line[j] === ESC ? j + 2 : j + 1;
    } else {
      i += 2;
    }
  }
  flush();
  return { segments, end: style };
}

export function stripAnsi(text: string): string {
  return scan(text, {})
    .segments.map((s) => s.text)
    .join("");
}

/** A line redrawn with carriage returns, like a progress bar, shows its last frame. */
function lastFrame(line: string): string {
  const frames = line.replace(/\r+$/, "").split("\r");
  for (let i = frames.length - 1; i >= 0; i--) if (stripAnsi(frames[i]).trim()) return frames[i];
  return frames[frames.length - 1];
}

export function parseLine(raw: string, start: AnsiStyle): Parsed {
  return scan(lastFrame(raw), start);
}

/** `cache` keeps lines that didn't change, so a growing log only parses its new lines. */
export function parseAnsi(text: string, cache?: Map<string, Parsed>): AnsiLine[] {
  const lines: AnsiLine[] = [];
  const used = new Map<string, Parsed>();
  let style: AnsiStyle = {};
  for (const raw of text.split("\n")) {
    const key = `${JSON.stringify(style)}\u0000${raw}`;
    const parsed = used.get(key) ?? cache?.get(key) ?? parseLine(raw, style);
    used.set(key, parsed);
    lines.push(parsed.segments);
    style = parsed.end;
  }
  if (cache) {
    cache.clear();
    for (const [key, value] of used) cache.set(key, value);
  }
  return lines;
}
