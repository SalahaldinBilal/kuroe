export interface Run {
  text: string;
  space: boolean;
  /** At the very start or end of the value, where whitespace is almost always a mistake. */
  edge: boolean;
}

const SPACE = /\s+/g;

export function spaceRuns(value: string): Run[] {
  const runs: Run[] = [];
  let last = 0;
  for (const match of value.matchAll(SPACE)) {
    const start = match.index ?? 0;
    if (start > last) runs.push({ text: value.slice(last, start), space: false, edge: false });
    const end = start + match[0].length;
    runs.push({ text: match[0], space: true, edge: start === 0 || end === value.length });
    last = end;
  }
  if (last < value.length) runs.push({ text: value.slice(last), space: false, edge: false });
  return runs;
}

function describe(run: string): string {
  if (/^ +$/.test(run)) return run.length === 1 ? "a space" : `${run.length} spaces`;
  if (/^\t+$/.test(run)) return run.length === 1 ? "a tab" : `${run.length} tabs`;
  if (/^[\r\n]+$/.test(run)) return "a line break";
  if (/^ +$/.test(run)) return "a non-breaking space";
  return "whitespace";
}

/** What looks wrong about a value's whitespace, or null when nothing does. */
export function whitespaceProblem(value: string): string | null {
  const lead = value.match(/^\s+/)?.[0];
  const trail = value.match(/\s+$/)?.[0];
  if (lead && lead === value) return `Only ${describe(lead)}`;
  const parts: string[] = [];
  if (lead) parts.push(`Starts with ${describe(lead)}`);
  if (trail) parts.push(`${parts.length ? "ends" : "Ends"} with ${describe(trail)}`);
  const inner = value.slice(lead?.length ?? 0, value.length - (trail?.length ?? 0));
  if (/[\t\r\n ]/.test(inner))
    parts.push(`${parts.length ? "has" : "Has"} a tab, line break or non-breaking space inside`);
  return parts.length ? parts.join(" and ") : null;
}
