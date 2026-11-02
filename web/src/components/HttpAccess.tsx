import { createSignal, Show } from "solid-js";
import { api } from "../api";
import useToastState from "../states/toastState";
import { Note } from "./ui/Blocks";
import Button from "./ui/Button/Button";
import Modal from "./ui/Modal/Modal";

export const onHttps = () => location.protocol === "https:";

/** Confirms turning plain HTTP off and shows how to get back in. */
export function HttpOffConfirm(props: { show: boolean; busy?: boolean; onConfirm: () => void; onHide: () => void }) {
  return (
    <Modal
      show={props.show}
      title="Turn off plain HTTP?"
      onHide={props.onHide}
      footer={
        <>
          <Button variant="ghost" onClick={props.onHide}>
            Cancel
          </Button>
          <Button variant="primary" disabled={props.busy} onClick={props.onConfirm}>
            Turn it off
          </Button>
        </>
      }
    >
      <div class="stack" style={{ gap: "12px" }}>
        <p>
          kuroe will only answer plain HTTP from this server and from HTTPS proxies. Everyone else has to use the HTTPS
          address.
        </p>
        <Note>
          <div class="stack" style={{ gap: "4px" }}>
            <span>If you lock yourself out, run this on the server:</span>
            <span class="mono">kuroe admin http open</span>
            <span>
              In the container install: <span class="mono">docker exec kuroe kuroe admin http open</span>
            </span>
          </div>
        </Note>
      </div>
    </Modal>
  );
}

/** Shown once after signing in while plain HTTP is still open. */
export function HttpBanner() {
  const { pushToast, pushError } = useToastState;
  const [hidden, setHidden] = createSignal(false);
  const [confirming, setConfirming] = createSignal(false);
  const [busy, setBusy] = createSignal(false);

  async function turnOff() {
    setBusy(true);
    try {
      await api.saveSettings({ http_access: "local" });
      pushToast("Plain HTTP is off");
      setConfirming(false);
      setHidden(true);
    } catch (err) {
      pushError(err);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Show when={!hidden()}>
      <div class="banner">
        <Show
          when={onHttps()}
          fallback={
            <Note
              tone="warn"
              action={
                <div class="hrow" style={{ gap: "8px" }}>
                  <Button size="sm" href="/proxy">
                    Proxy settings
                  </Button>
                  <Button size="sm" variant="ghost" onClick={() => setHidden(true)}>
                    Not now
                  </Button>
                </div>
              }
            >
              kuroe is served over plain HTTP. Set up HTTPS, then turn plain HTTP off.
            </Note>
          }
        >
          <Note
            tone="warn"
            action={
              <div class="hrow" style={{ gap: "8px" }}>
                <Button size="sm" variant="primary" onClick={() => setConfirming(true)}>
                  Turn it off
                </Button>
                <Button size="sm" variant="ghost" onClick={() => setHidden(true)}>
                  Not now
                </Button>
              </div>
            }
          >
            You're on HTTPS. Turn off plain HTTP so nobody signs in unencrypted.
          </Note>
        </Show>
      </div>
      <HttpOffConfirm show={confirming()} busy={busy()} onConfirm={turnOff} onHide={() => setConfirming(false)} />
    </Show>
  );
}
