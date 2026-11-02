import { createSignal, Show } from "solid-js";
import { api } from "../api";
import useToastState, { errorMessage } from "../states/toastState";
import { Note } from "./ui/Blocks";
import Button from "./ui/Button/Button";
import Field from "./ui/Field/Field";
import Input from "./ui/Input/Input";
import Modal from "./ui/Modal/Modal";

export function ChangePassword(props: { show: boolean; onHide: () => void }) {
  const { pushToast } = useToastState;
  const [current, setCurrent] = createSignal("");
  const [next, setNext] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  function close() {
    setCurrent("");
    setNext("");
    setError("");
    props.onHide();
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    setError("");
    setBusy(true);
    try {
      await api.changePassword(current(), next());
      pushToast("Password changed. Your other sessions were signed out.");
      close();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal
      show={props.show}
      title="Change your password"
      onHide={close}
      footer={
        <>
          <Button variant="ghost" onClick={close}>
            Cancel
          </Button>
          <Button type="submit" form="change-password" variant="primary" disabled={busy()}>
            Change password
          </Button>
        </>
      }
    >
      <form id="change-password" class="stack" style={{ gap: "14px" }} onSubmit={save}>
        <Field label="Current password">
          <Input
            autofocus
            required
            type="password"
            label="Current password"
            autocomplete="current-password"
            value={current()}
            onValue={setCurrent}
          />
        </Field>
        <Field label="New password" hint="At least 10 characters. Other places you're signed in are signed out.">
          <Input
            required
            type="password"
            minLength={10}
            label="New password"
            autocomplete="new-password"
            value={next()}
            onValue={setNext}
          />
        </Field>
        <Show when={error()}>
          <Note tone="bad">{error()}</Note>
        </Show>
      </form>
    </Modal>
  );
}
