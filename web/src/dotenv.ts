import type { EnvRow } from "./spec";

export interface DotenvEntry {
  key: string;
  value: string;
}

export interface Parsed {
  entries: DotenvEntry[];
  invalid: number;
}

const KEY = /^[A-Za-z_][A-Za-z0-9_.]*$/;
const SECRET = /SECRET|TOKEN|PASSWORD|PASSWD|PRIVATE|_KEY$|^KEY$|DSN$/i;
const ESCAPES: Record<string, string> = { n: "\n", r: "\r", t: "\t", '"': '"', "\\": "\\" };

export function looksSecret(key: string): boolean {
  return SECRET.test(key);
}

function unescapeDouble(text: string): string {
  return text.replace(/\\(.)/g, (whole, c: string) => ESCAPES[c] ?? whole);
}

function closingQuote(text: string, quote: string, from: number): number {
  for (let i = from; i < text.length; i++) {
    if (quote === '"' && text[i] === "\\") i++;
    else if (text[i] === quote) return i;
  }
  return -1;
}

export function parseDotenv(text: string): Parsed {
  const lines = text.replace(/\r\n?/g, "\n").split("\n");
  const found = new Map<string, string>();
  let invalid = 0;
  for (let i = 0; i < lines.length; i++) {
    let line = lines[i].trim();
    if (!line || line.startsWith("#")) continue;
    if (line.startsWith("export ")) line = line.slice(7).trimStart();
    const eq = line.indexOf("=");
    const key = eq < 0 ? "" : line.slice(0, eq).trim();
    if (!KEY.test(key)) {
      invalid++;
      continue;
    }
    let rest = line.slice(eq + 1).trimStart();
    const quote = rest[0];
    let value: string;
    if (quote === '"' || quote === "'") {
      let end = closingQuote(rest, quote, 1);
      while (end < 0 && i + 1 < lines.length) {
        rest += `\n${lines[++i]}`;
        end = closingQuote(rest, quote, 1);
      }
      if (end < 0) {
        invalid++;
        continue;
      }
      value = rest.slice(1, end);
      if (quote === '"') value = unescapeDouble(value);
    } else {
      value = rest.replace(/\s+#.*$/, "").trim();
    }
    found.set(key, value);
  }
  return { entries: [...found].map(([key, value]) => ({ key, value })), invalid };
}

export interface Merged {
  rows: EnvRow[];
  added: number;
  updated: number;
}

export function mergeEnv(rows: EnvRow[], entries: DotenvEntry[]): Merged {
  const next = rows.filter((r) => r.key.trim() || r.value || r.ref).map((r) => ({ ...r }));
  let added = 0;
  let updated = 0;
  for (const { key, value } of entries) {
    const existing = next.find((r) => r.key.trim() === key);
    if (existing) {
      existing.value = value;
      existing.secret ||= looksSecret(key) || !!existing.ref;
      existing.ref = null;
      updated++;
    } else {
      next.push({ key, value, secret: looksSecret(key), ref: null });
      added++;
    }
  }
  return { rows: next, added, updated };
}

export function mergeSummary(merged: Merged): string {
  const vars = (n: number) => `${n} ${n === 1 ? "variable" : "variables"}`;
  if (merged.updated === 0) return `Added ${vars(merged.added)}`;
  if (merged.added === 0) return `Updated ${vars(merged.updated)}`;
  return `Added ${vars(merged.added)}, updated ${merged.updated}`;
}
