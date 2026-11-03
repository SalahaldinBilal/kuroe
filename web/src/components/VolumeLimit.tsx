import { createEffect, createSignal, Show } from "solid-js";
import { api, type VolumeInfo } from "../api";
import { formatAgo, formatBytes } from "../format";
import useToastState from "../states/toastState";
import { Pill } from "./ui/Blocks";
import Button from "./ui/Button/Button";
import Field, { FieldRow } from "./ui/Field/Field";
import Input from "./ui/Input/Input";
import Modal from "./ui/Modal/Modal";
import Select from "./ui/Select/Select";
import Switch from "./ui/Switch/Switch";
import Tip from "./ui/Tooltip/Tip";

const MIB = 2 ** 20;
const GIB = 2 ** 30;
type Unit = "MiB" | "GiB";

export function volumeLabel(v: VolumeInfo): string {
  return v.shared ?? v.name;
}

export function SizeCell(props: { volume: VolumeInfo | null | undefined }) {
  return (
    <Show when={props.volume?.bytes != null} fallback={<span class="faint">Not measured yet</span>}>
      <span class="hrow" style={{ gap: "6px" }}>
        <Tip content={`Measured ${formatAgo(props.volume?.measured_at ?? 0)}`}>
          <span class="mono">{formatBytes(props.volume?.bytes ?? 0)}</span>
        </Tip>
        <Show when={props.volume?.over}>
          <Pill tone="bad">over limit</Pill>
        </Show>
      </span>
    </Show>
  );
}

export function LimitCell(props: { volume: VolumeInfo | null | undefined }) {
  return (
    <Show when={props.volume?.limit} fallback={<span class="faint">None</span>}>
      {(limit) => (
        <span class="mono">
          {formatBytes(limit().max_bytes)}
          <Show when={limit().stop}>
            <span class="faint"> · stops</span>
          </Show>
        </span>
      )}
    </Show>
  );
}

export function LimitModal(props: { volume: VolumeInfo | null; onHide: () => void; onSaved: () => void }) {
  const { pushToast, pushError } = useToastState;
  const [amount, setAmount] = createSignal("");
  const [unit, setUnit] = createSignal<Unit>("GiB");
  const [stop, setStop] = createSignal(false);
  const [busy, setBusy] = createSignal(false);

  createEffect(() => {
    const limit = props.volume?.limit;
    const inGib = !limit || limit.max_bytes % GIB === 0;
    setUnit(inGib ? "GiB" : "MiB");
    setAmount(limit ? String(+(limit.max_bytes / (inGib ? GIB : MIB)).toFixed(2)) : "");
    setStop(limit?.stop ?? false);
  });

  async function run(action: () => Promise<void>, done: string) {
    setBusy(true);
    try {
      await action();
      pushToast(done);
      props.onSaved();
      props.onHide();
    } catch (err) {
      pushError(err);
    } finally {
      setBusy(false);
    }
  }

  function save() {
    const v = props.volume;
    const n = Number(amount());
    if (!v) return;
    if (!(n > 0)) return pushError("Give a limit above 0");
    const max_bytes = Math.round(n * (unit() === "GiB" ? GIB : MIB));
    run(() => api.setVolumeLimit(v.name, { max_bytes, stop: stop() }), `Limit set on ${volumeLabel(v)}`);
  }

  return (
    <Modal
      show={props.volume != null}
      title={`Limit for ${props.volume ? volumeLabel(props.volume) : ""}`}
      onHide={props.onHide}
      footer={
        <>
          <Show when={props.volume?.limit}>
            <Button
              variant="ghost"
              disabled={busy()}
              onClick={() => {
                const v = props.volume;
                if (v) run(() => api.removeVolumeLimit(v.name), `Limit removed from ${volumeLabel(v)}`);
              }}
            >
              Remove limit
            </Button>
          </Show>
          <span class="grow" />
          <Button variant="ghost" onClick={props.onHide}>
            Cancel
          </Button>
          <Button type="submit" form="volume-limit" variant="primary" disabled={busy()}>
            Save limit
          </Button>
        </>
      }
    >
      <form
        id="volume-limit"
        class="stack"
        style={{ gap: "14px" }}
        onSubmit={(e) => {
          e.preventDefault();
          save();
        }}
      >
        <p class="muted" style={{ margin: "0" }}>
          Docker can't cap a volume's size, so kuroe checks it against this limit each time it measures, every 10
          minutes. It's {formatBytes(props.volume?.bytes ?? 0)} now.
        </p>
        <FieldRow>
          <Field label="Limit" grow>
            <Input
              mono
              autofocus
              inputMode="decimal"
              label="Limit"
              placeholder="10"
              value={amount()}
              onValue={setAmount}
            />
          </Field>
          <Field label="Unit" width={110}>
            <Select
              label="Unit"
              value={unit()}
              onChange={setUnit}
              items={[
                { value: "MiB" as const, label: "MiB" },
                { value: "GiB" as const, label: "GiB" },
              ]}
            />
          </Field>
        </FieldRow>
        <Field
          label="When it's over"
          hint="The volume and deployment pages always show that it's over. Stopping keeps the deployments down until you free up space or raise the limit, then deploy again."
        >
          <Switch label="Stop the deployments that use it" checked={stop()} onChange={setStop} />
        </Field>
      </form>
    </Modal>
  );
}
