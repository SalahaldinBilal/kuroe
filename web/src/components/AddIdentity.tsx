import { createSignal, Match, Show, Switch } from "solid-js";
import { api, type IdentityKind } from "../api";
import { PROVIDERS, type Provider, postManifest } from "../identities";
import useToastState, { errorMessage } from "../states/toastState";
import { Note } from "./ui/Blocks";
import Button from "./ui/Button/Button";
import Field, { FieldRow } from "./ui/Field/Field";
import { Tiles } from "./ui/Form";
import { Icon } from "./ui/Icon";
import Input from "./ui/Input/Input";
import Modal from "./ui/Modal/Modal";
import SegmentedControl from "./ui/SegmentedControl/SegmentedControl";

const TOKEN_HINT: Partial<Record<Provider, string>> = {
  github: "Fine-grained token with Contents: read. Add Packages: read to pull from ghcr.io.",
  gitlab: "Needs read_repository. Add read_registry to pull images.",
  gitea: "Needs read access to repositories.",
  cloudflare:
    "Create one in Cloudflare with Zone, DNS, Edit for the zones you use. Add Zone, SSL and Certificates, Edit to make origin certificates.",
};

function kindFor(provider: Provider, githubApp: boolean): IdentityKind {
  if (provider === "github") return githubApp ? "github_app" : "github_token";
  return provider;
}

export function AddIdentity(props: {
  provider: Provider | null;
  onCertificate: () => void;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { pushToast } = useToastState;
  const [provider, setProvider] = createSignal<Provider | null>(props.provider);
  const [method, setMethod] = createSignal<"app" | "token">("app");
  const [name, setName] = createSignal("");
  const [host, setHost] = createSignal(props.provider === "gitlab" ? "https://gitlab.com" : "");
  const [username, setUsername] = createSignal("");
  const [secret, setSecret] = createSignal("");
  const [owner, setOwner] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const info = () => PROVIDERS.find((p) => p.value === provider());
  const githubApp = () => provider() === "github" && method() === "app";

  function pick(p: Provider) {
    if (p === "certificate") return props.onCertificate();
    setProvider(p);
    setHost(p === "gitlab" ? "https://gitlab.com" : "");
    setError("");
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    const p = provider();
    if (!p) return;
    setError("");
    setBusy(true);
    try {
      const label = name().trim() || info()?.label || p;
      if (githubApp()) {
        const started = await api.startGithubApp(label, owner().trim());
        postManifest(started.action, started.manifest);
        return;
      }
      await api.createIdentity({
        kind: kindFor(p, false),
        name: label,
        host: host(),
        username: username(),
        secret: secret(),
      });
      pushToast(`Saved ${label}`);
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
      width={560}
      title={info() ? `Add ${info()?.label}` : "Add identity"}
      onHide={props.onClose}
      footer={
        provider() ? (
          <>
            <Button variant="ghost" onClick={() => setProvider(null)}>
              <Icon name="back" />
              Back
            </Button>
            <span class="grow" />
            <Button variant="ghost" onClick={props.onClose}>
              Cancel
            </Button>
            <Button type="submit" form="add-identity" variant="primary" disabled={busy()}>
              <Show when={githubApp()} fallback="Save">
                Continue on GitHub
                <Icon name="ext" />
              </Show>
            </Button>
          </>
        ) : undefined
      }
    >
      <Show
        when={provider()}
        fallback={
          <Tiles
            label="Provider"
            columns={2}
            value={"" as Provider}
            onChange={pick}
            options={PROVIDERS.map((p) => ({ value: p.value, label: p.label, description: p.description }))}
          />
        }
      >
        {(p) => (
          <form id="add-identity" class="stack" style={{ gap: "14px" }} onSubmit={save}>
            <Field label="Name" hint="Shown when you pick it on a deployment.">
              <Input autofocus label="Name" placeholder={info()?.label} value={name()} onValue={setName} />
            </Field>
            <Switch>
              <Match when={p() === "github"}>
                <SegmentedControl
                  label="GitHub method"
                  value={method()}
                  onChange={setMethod}
                  options={[
                    { value: "app", label: "GitHub App" },
                    { value: "token", label: "Personal access token" },
                  ]}
                />
                <Show
                  when={method() === "app"}
                  fallback={<SecretField hint={TOKEN_HINT.github} value={secret()} onValue={setSecret} />}
                >
                  <Note>
                    Opens GitHub with the app settings filled in. You pick which repositories it can read. kuroe can
                    then list repos and receive pushes itself.
                  </Note>
                  <Field label="Organization" hint="Leave empty to create the app on your own account.">
                    <Input mono label="Organization" placeholder="acme" value={owner()} onValue={setOwner} />
                  </Field>
                </Show>
              </Match>
              <Match when={p() === "gitlab" || p() === "gitea"}>
                <Field label="Instance">
                  <Input mono label="Instance" placeholder="https://git.example.com" value={host()} onValue={setHost} />
                </Field>
                <SecretField hint={TOKEN_HINT[p()]} value={secret()} onValue={setSecret} />
              </Match>
              <Match when={p() === "git" || p() === "registry"}>
                <Field label={p() === "git" ? "Host" : "Registry"}>
                  <Input
                    mono
                    label={p() === "git" ? "Host" : "Registry"}
                    placeholder={p() === "git" ? "git.example.com" : "docker.io, ghcr.io, registry.example.com"}
                    value={host()}
                    onValue={setHost}
                  />
                </Field>
                <FieldRow>
                  <Field label="Username">
                    <Input label="Username" value={username()} onValue={setUsername} />
                  </Field>
                  <Field label={p() === "git" ? "Token or password" : "Password or token"}>
                    <Input type="password" label="Password" value={secret()} onValue={setSecret} />
                  </Field>
                </FieldRow>
              </Match>
              <Match when={p() === "cloudflare"}>
                <SecretField hint={TOKEN_HINT.cloudflare} value={secret()} onValue={setSecret} />
                <Note tone="ok">kuroe checks the token and lists the zones it can reach before saving.</Note>
              </Match>
            </Switch>
            <Show when={error()}>
              <Note tone="bad">{error()}</Note>
            </Show>
          </form>
        )}
      </Show>
    </Modal>
  );
}

function SecretField(props: { hint?: string; value: string; onValue: (v: string) => void }) {
  return (
    <Field label="Token" hint={props.hint}>
      <Input type="password" mono label="Token" value={props.value} onValue={props.onValue} />
    </Field>
  );
}
