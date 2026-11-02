import { createSignal, Show } from "solid-js";
import { api, type ProxyMode, type UiDomain } from "../api";
import useToastState from "../states/toastState";
import { Card, Note } from "./ui/Blocks";
import Button from "./ui/Button/Button";
import { Icon } from "./ui/Icon";
import Input from "./ui/Input/Input";
import SettingRow from "./ui/SettingRow/SettingRow";
import Switch from "./ui/Switch/Switch";

interface Props {
  saved: UiDomain | null;
  mode: ProxyMode;
  disabled: boolean;
  onSaved: () => void;
}

export function uiUrl(ui: UiDomain): string {
  return `${ui.tls ? "https" : "http"}://${ui.hostname}`;
}

export function OwnDomain(props: Props) {
  const { pushToast, pushError } = useToastState;
  const [hostname, setHostname] = createSignal(props.saved?.hostname ?? "");
  const [tls, setTls] = createSignal(props.saved?.tls ?? true);
  const [busy, setBusy] = createSignal(false);
  const [check, setCheck] = createSignal<{ ok: boolean; message: string } | null>(null);

  const trimmed = () => hostname().trim().toLowerCase();
  const changed = () =>
    trimmed() !== (props.saved?.hostname ?? "") || (trimmed() !== "" && tls() !== (props.saved?.tls ?? true));

  async function save() {
    setBusy(true);
    setCheck(null);
    try {
      const result = await api.saveUiDomain({ hostname: trimmed(), tls: tls() });
      pushToast(
        result.ui
          ? `Saved. kuroe answers at ${uiUrl(result.ui)} once its DNS points here.`
          : "kuroe no longer has its own domain",
      );
      props.onSaved();
    } catch (err) {
      pushError(err);
    } finally {
      setBusy(false);
    }
  }

  async function runCheck() {
    setBusy(true);
    try {
      setCheck(await api.checkUiDomain());
    } catch (err) {
      pushError(err);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card
      title="kuroe's own domain"
      description={
        props.mode === "managed"
          ? "Reach this page on a domain of its own, through Traefik, with the same certificates as your deployments."
          : "Your web server forwards this domain to kuroe. It's written with your other domains."
      }
    >
      <SettingRow label="Domain" description="Point its DNS record at this server first.">
        <Input
          mono
          label="kuroe's domain"
          placeholder="kuroe.example.com"
          disabled={props.disabled}
          value={hostname()}
          onValue={setHostname}
        />
      </SettingRow>
      <SettingRow label="HTTPS" description="Plain HTTP on this domain redirects to HTTPS.">
        <Switch checked={tls()} disabled={props.disabled || !trimmed()} onChange={setTls} />
      </SettingRow>
      <Show when={check()}>
        {(c) => (
          <div class="card-b" style={{ "border-top": "1px solid var(--line-soft)" }}>
            <Note tone={c().ok ? "ok" : "bad"}>{c().message}</Note>
          </div>
        )}
      </Show>
      <div class="card-b hrow" style={{ "border-top": "1px solid var(--line-soft)", gap: "8px" }}>
        <span class="hint grow">
          {props.saved ? (
            <>
              kuroe answers at{" "}
              <a class="mono" href={uiUrl(props.saved)} target="_blank" rel="noreferrer">
                {uiUrl(props.saved)}
              </a>
            </>
          ) : (
            "The address you use now keeps working either way."
          )}
        </span>
        <Show when={props.saved && !changed()}>
          <Button size="sm" disabled={busy()} onClick={runCheck}>
            <Icon name="check" />
            Check
          </Button>
        </Show>
        <Show when={changed()}>
          <Button size="sm" variant="primary" disabled={busy() || props.disabled} onClick={save}>
            {trimmed() ? "Save" : "Remove"}
          </Button>
        </Show>
      </div>
    </Card>
  );
}
