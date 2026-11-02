import { createResource, createSignal, Show } from "solid-js";
import { api } from "../api";
import { zoneHostnames } from "../certificates";
import { formatDate } from "../format";
import useToastState, { errorMessage } from "../states/toastState";
import { Note } from "./ui/Blocks";
import Button from "./ui/Button/Button";
import Field from "./ui/Field/Field";
import Input from "./ui/Input/Input";
import Modal from "./ui/Modal/Modal";
import { ready } from "./ui/Overlay";
import SegmentedControl from "./ui/SegmentedControl/SegmentedControl";
import Select from "./ui/Select/Select";

type Way = "paste" | "cloudflare";

export function AddCertificate(props: { onClose: () => void; onSaved: () => void; onBack?: () => void }) {
  const { pushToast } = useToastState;
  const [way, setWay] = createSignal<Way>("paste");
  const [name, setName] = createSignal("");
  const [chain, setChain] = createSignal("");
  const [key, setKey] = createSignal("");
  const [cloudflare, setCloudflare] = createSignal("");
  const [zone, setZone] = createSignal("");
  const [hostnames, setHostnames] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [identities] = createResource(api.identities);

  const logins = () => (ready(identities) ?? []).filter((i) => i.kind === "cloudflare");
  const zones = () => {
    const z = logins().find((l) => l.id === cloudflare())?.meta.zones;
    return Array.isArray(z) ? (z as string[]) : [];
  };

  function pickLogin(id: string) {
    setCloudflare(id);
    const first = (logins().find((l) => l.id === id)?.meta.zones as string[] | undefined)?.[0] ?? "";
    pickZone(first);
  }

  function pickZone(z: string) {
    setZone(z);
    setHostnames(zoneHostnames(z).join(", "));
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    setError("");
    setBusy(true);
    try {
      if (way() === "paste") {
        const saved = await api.addCertificate(name(), chain(), key());
        const origin = saved.cloudflare_origin ? " Cloudflare origin certificates need Cloudflare's proxy on." : "";
        pushToast(`Saved. Covers ${saved.sans.join(", ")} until ${formatDate(saved.not_after)}.${origin}`);
      } else {
        const list = hostnames()
          .split(",")
          .map((h) => h.trim())
          .filter(Boolean);
        const saved = await api.createOriginCertificate(name(), cloudflare(), list);
        pushToast(`Cloudflare issued it. Covers ${saved.sans.join(", ")} until ${formatDate(saved.not_after)}.`);
      }
      props.onSaved();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal
      show
      width={600}
      title="Add a certificate"
      onHide={props.onClose}
      footer={
        <>
          <Show when={props.onBack}>
            <Button variant="ghost" onClick={() => props.onBack?.()}>
              Back
            </Button>
          </Show>
          <span class="grow" />
          <Button variant="ghost" onClick={props.onClose}>
            Cancel
          </Button>
          <Button type="submit" form="add-certificate" variant="primary" disabled={busy()}>
            {way() === "paste" ? "Save" : "Create at Cloudflare"}
          </Button>
        </>
      }
    >
      <form id="add-certificate" class="stack" style={{ gap: "14px" }} onSubmit={save}>
        <div>
          <SegmentedControl
            label="How"
            value={way()}
            onChange={setWay}
            options={[
              { value: "paste", label: "Paste a certificate" },
              { value: "cloudflare", label: "Cloudflare origin certificate" },
            ]}
          />
        </div>
        <Field label="Name" hint="Leave empty to name it after its first hostname.">
          <Input label="Name" placeholder="example.com" value={name()} onValue={setName} />
        </Field>
        <Show
          when={way() === "paste"}
          fallback={
            <Show
              when={logins().length > 0}
              fallback={
                <Note tone="warn">
                  Add a Cloudflare identity first. Its token needs Zone, SSL and Certificates, Edit.
                </Note>
              }
            >
              <Field label="Cloudflare identity" hint="Its token needs Zone, SSL and Certificates, Edit.">
                <Select
                  label="Cloudflare identity"
                  placeholder="Pick one"
                  items={logins().map((l) => ({ value: l.id, label: l.name }))}
                  value={cloudflare()}
                  onChange={pickLogin}
                />
              </Field>
              <Field label="Zone">
                <Select
                  mono
                  label="Zone"
                  placeholder={cloudflare() ? "No zones, run Test on the identity" : "Pick an identity first"}
                  items={zones().map((z) => ({ value: z, label: z }))}
                  value={zone()}
                  onChange={pickZone}
                />
              </Field>
              <Field
                label="Hostnames"
                hint="Separated by commas. The apex and *.zone cover every first-level subdomain."
              >
                <Input mono label="Hostnames" value={hostnames()} onValue={setHostnames} />
              </Field>
              <Note>
                kuroe makes the private key here and sends Cloudflare only a signing request, so the key never leaves
                this server. The certificate lasts 15 years and only works with Cloudflare's proxy on and SSL set to
                Full (strict).
              </Note>
            </Show>
          }
        >
          <Field label="Certificate" hint="The certificate first, then any intermediates.">
            <textarea
              class="ta"
              rows={5}
              aria-label="Certificate"
              placeholder="-----BEGIN CERTIFICATE-----"
              value={chain()}
              onInput={(e) => setChain(e.currentTarget.value)}
            />
          </Field>
          <Field label="Private key" hint="Stored on this server only. Left out of exports.">
            <textarea
              class="ta"
              rows={5}
              aria-label="Private key"
              placeholder="-----BEGIN PRIVATE KEY-----"
              value={key()}
              onInput={(e) => setKey(e.currentTarget.value)}
            />
          </Field>
        </Show>
        <Show when={error()}>
          <Note tone="bad">{error()}</Note>
        </Show>
      </form>
    </Modal>
  );
}
