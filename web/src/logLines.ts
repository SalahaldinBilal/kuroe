import { type AnsiLine, parseLine, stripAnsi } from "./ansi";

export interface LogEntry {
  /** Docker's RFC 3339 time, missing on lines kuroe didn't write. */
  stamp?: string;
  stream?: "stdout" | "stderr";
  container?: string;
  segments: AnsiLine;
  /** The text without colors, lowercased, for the filter. */
  search: string;
}

const PREFIX = /^(\d{4}-\d{2}-\d{2}T[\d:.]+Z) (stdout|stderr) ([0-9a-f]{1,12})(?: |$)/;

export function parseLogLine(raw: string): LogEntry {
  const match = PREFIX.exec(raw);
  const text = match ? raw.slice(match[0].length) : raw;
  return {
    stamp: match?.[1],
    stream: match?.[2] as LogEntry["stream"],
    container: match?.[3],
    segments: parseLine(text, {}).segments,
    search: stripAnsi(text).toLowerCase(),
  };
}

/** Complete lines from text that may end mid-line, and the unfinished rest. */
export function splitLines(text: string): { lines: string[]; rest: string } {
  const end = text.lastIndexOf("\n");
  if (end < 0) return { lines: [], rest: text };
  return { lines: text.slice(0, end).split("\n"), rest: text.slice(end + 1) };
}

export function clockTime(stamp: string): string {
  return stamp.slice(11, 19);
}

export function stampText(entry: LogEntry): string {
  if (!entry.stamp) return "";
  return `${entry.stamp.slice(0, 10)} ${entry.stamp.slice(11, 23)} UTC · ${entry.stream} · container ${entry.container}`;
}
