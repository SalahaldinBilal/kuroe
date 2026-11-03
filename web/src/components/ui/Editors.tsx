import { createMemo, createSignal, Index, Show } from "solid-js";
import { type DotenvEntry, mergeEnv, mergeSummary, parseDotenv } from "../../dotenv";
import { type EnvRow, emptyEnv, emptyPort, type MountRow, type PortRow } from "../../spec";
import useToastState from "../../states/toastState";
import { SecretValue } from "../SecretValue";
import Button from "./Button/Button";
import { Icon } from "./Icon";
import Input from "./Input/Input";
import Select, { items } from "./Select/Select";
import Switch from "./Switch/Switch";

function patch<T>(rows: T[], i: number, change: Partial<T>): T[] {
  return rows.map((r, j) => (j === i ? { ...r, ...change } : r));
}

function RemoveButton(props: { onClick: () => void }) {
  return (
    <Button variant="ghost" icon size="sm" tooltip="Remove" onClick={props.onClick}>
      <Icon name="x" />
    </Button>
  );
}

function AddButton(props: { label: string; onClick: () => void }) {
  return (
    <div class="kv-actions">
      <Button size="sm" onClick={props.onClick}>
        <Icon name="plus" />
        {props.label}
      </Button>
    </div>
  );
}

interface PairRowsProps {
  rows: { key: string; value: string }[];
  keyLabel: string;
  valueLabel: string;
  addLabel: string;
  onChange: (rows: { key: string; value: string }[]) => void;
}

/** Plain name and value rows, like query params or headers. */
export function PairRows(props: PairRowsProps) {
  return (
    <div class="kv">
      <Index each={props.rows}>
        {(row, i) => (
          <div class="kvrow">
            <Input
              mono
              class="k"
              label={props.keyLabel}
              placeholder={props.keyLabel}
              value={row().key}
              onValue={(key) => props.onChange(patch(props.rows, i, { key }))}
            />
            <Input
              mono
              label={props.valueLabel}
              placeholder={props.valueLabel}
              value={row().value}
              onValue={(value) => props.onChange(patch(props.rows, i, { value }))}
            />
            <RemoveButton onClick={() => props.onChange(props.rows.filter((_, j) => j !== i))} />
          </div>
        )}
      </Index>
      <AddButton label={props.addLabel} onClick={() => props.onChange([...props.rows, { key: "", value: "" }])} />
    </div>
  );
}

const PROTOCOLS = items(["tcp", "udp"]);

export function PortRows(props: { rows: PortRow[]; onChange: (rows: PortRow[]) => void }) {
  return (
    <div class="kv">
      <Index each={props.rows}>
        {(row, i) => (
          <div class="kvrow">
            <Input
              mono
              class="k"
              inputMode="numeric"
              label="Host port"
              placeholder="host port"
              value={row().host}
              onValue={(host) => props.onChange(patch(props.rows, i, { host }))}
            />
            <span class="arrow">
              <Icon name="right" />
            </span>
            <Input
              mono
              inputMode="numeric"
              label="Container port"
              placeholder="container port"
              value={row().container}
              onValue={(container) => props.onChange(patch(props.rows, i, { container }))}
            />
            <Select
              mono
              class="proto"
              label="Protocol"
              items={PROTOCOLS}
              value={row().protocol}
              onChange={(protocol) =>
                props.onChange(patch(props.rows, i, { protocol: protocol as PortRow["protocol"] }))
              }
            />
            <RemoveButton onClick={() => props.onChange(props.rows.filter((_, j) => j !== i))} />
          </div>
        )}
      </Index>
      <AddButton label="Add port" onClick={() => props.onChange([...props.rows, emptyPort()])} />
    </div>
  );
}

export function EnvRows(props: { rows: EnvRow[]; names?: Record<string, string>; onChange: (rows: EnvRow[]) => void }) {
  const { pushToast, pushError } = useToastState;
  const [pasting, setPasting] = createSignal(false);
  const [text, setText] = createSignal("");
  const parsed = createMemo(() => parseDotenv(text()));

  function apply(entries: DotenvEntry[]) {
    const merged = mergeEnv(props.rows, entries);
    props.onChange(merged.rows);
    pushToast(mergeSummary(merged));
  }

  function addPasted() {
    const { entries } = parsed();
    if (entries.length === 0) {
      pushError("No variables found. Lines should look like KEY=value.");
      return;
    }
    apply(entries);
    setText("");
    setPasting(false);
  }

  function onPaste(event: ClipboardEvent, keyField: boolean) {
    const pasted = event.clipboardData?.getData("text") ?? "";
    if (!pasted.includes("\n") && !(keyField && pasted.includes("="))) return;
    const { entries, invalid } = parseDotenv(pasted);
    if (entries.length === 0 || invalid > 0) return;
    event.preventDefault();
    apply(entries);
  }

  const count = () => {
    const { entries, invalid } = parsed();
    const found = `${entries.length} ${entries.length === 1 ? "variable" : "variables"} found`;
    return invalid > 0 ? `${found}, ${invalid} ${invalid === 1 ? "line" : "lines"} skipped` : found;
  };

  return (
    <div class="kv">
      <span class="hint">Raw values live on this deployment. Saved secrets come from the Secrets page.</span>
      <Index each={props.rows}>
        {(row, i) => (
          <div class="kvrow">
            <Input
              mono
              class="k"
              label="Key"
              placeholder="KEY"
              value={row().key}
              onValue={(key) => props.onChange(patch(props.rows, i, { key }))}
              onPaste={(e) => onPaste(e, true)}
            />
            <SecretValue
              label="Value"
              value={row().value}
              secretRef={row().ref}
              secret={row().secret}
              names={props.names}
              onChange={(next) => props.onChange(patch(props.rows, i, next))}
              onPaste={(e) => onPaste(e, false)}
            />
            <Button
              variant="ghost"
              icon
              size="sm"
              active={row().secret || !!row().ref}
              disabled={!!row().ref}
              tooltip={
                row().ref
                  ? "Saved secrets are always secret"
                  : row().secret
                    ? "Secret: hidden from viewers and left out of exports"
                    : "Mark as secret"
              }
              onClick={() => props.onChange(patch(props.rows, i, { secret: !row().secret }))}
            >
              <Icon name="lock" />
            </Button>
            <RemoveButton onClick={() => props.onChange(props.rows.filter((_, j) => j !== i))} />
          </div>
        )}
      </Index>
      <Show when={pasting()}>
        <div class="env-paste">
          <textarea
            class="ta"
            rows={6}
            aria-label="Paste .env"
            placeholder={'DATABASE_URL=postgres://db/app\n# comments are skipped\nexport API_TOKEN="abc"'}
            value={text()}
            onInput={(e) => setText(e.currentTarget.value)}
          />
          <div class="kv-actions">
            <span class="hint grow">{count()}</span>
            <Button
              size="sm"
              onClick={() => {
                setText("");
                setPasting(false);
              }}
            >
              Cancel
            </Button>
            <Button size="sm" variant="primary" disabled={parsed().entries.length === 0} onClick={addPasted}>
              Add variables
            </Button>
          </div>
        </div>
      </Show>
      <div class="kv-actions">
        <Button size="sm" onClick={() => props.onChange([...props.rows, emptyEnv()])}>
          <Icon name="plus" />
          Add variable
        </Button>
        <Show when={!pasting()}>
          <Button size="sm" onClick={() => setPasting(true)}>
            <Icon name="file" />
            Paste .env
          </Button>
        </Show>
      </div>
    </div>
  );
}

interface MountRowsProps {
  rows: MountRow[];
  volumes: string[];
  onChange: (rows: MountRow[]) => void;
}

export function MountRows(props: MountRowsProps) {
  return (
    <div class="kv">
      <Index each={props.rows}>
        {(row, i) => (
          <div class="kvrow">
            <Select
              mono
              class="k"
              label="Volume"
              items={items(props.volumes.includes(row().volume) ? props.volumes : [row().volume, ...props.volumes])}
              value={row().volume}
              onChange={(volume) => props.onChange(patch(props.rows, i, { volume }))}
            />
            <Input
              mono
              label="Path in the container"
              placeholder="/path/in/container"
              value={row().target}
              onValue={(target) => props.onChange(patch(props.rows, i, { target }))}
            />
            <Switch
              label="Read-only"
              checked={row().readOnly}
              onChange={(readOnly) => props.onChange(patch(props.rows, i, { readOnly }))}
            />
            <RemoveButton onClick={() => props.onChange(props.rows.filter((_, j) => j !== i))} />
          </div>
        )}
      </Index>
      <AddButton
        label="Add mount"
        onClick={() => props.onChange([...props.rows, { volume: props.volumes[0] ?? "", target: "", readOnly: false }])}
      />
    </div>
  );
}
