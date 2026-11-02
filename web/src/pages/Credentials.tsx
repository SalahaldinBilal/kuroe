import { createSignal, Show } from "solid-js";
import type { User } from "../api";
import { Note } from "../components/ui/Blocks";
import Button from "../components/ui/Button/Button";
import Field from "../components/ui/Field/Field";
import Input from "../components/ui/Input/Input";
import { Logo } from "../components/ui/Logo";
import { errorMessage } from "../states/toastState";

interface Props {
  title: string;
  hint: string;
  submit: string;
  newPassword?: boolean;
  setupCode?: boolean;
  action: (username: string, password: string, code: string) => Promise<User>;
  onDone: (user: User) => void;
}

export function Credentials(props: Props) {
  const [username, setUsername] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [code, setCode] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  async function onSubmit(event: SubmitEvent) {
    event.preventDefault();
    setBusy(true);
    setError("");
    try {
      props.onDone(await props.action(username(), password(), code()));
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div class="auth">
      <div class="auth-card">
        <div class="brand">
          <Logo />
          kuroe
        </div>
        <h1>{props.title}</h1>
        <p class="sub">{props.hint}</p>
        <form onSubmit={onSubmit}>
          <Show when={props.setupCode}>
            <Field
              label="Setup code"
              hint={
                <>
                  The installer printed it. Run <span class="mono">kuroe admin setup-code</span> on the server to see it
                  again.
                </>
              }
            >
              <Input
                autofocus
                required
                mono
                autocomplete="off"
                placeholder="XXXX-XXXX-XXXX"
                value={code()}
                onValue={setCode}
              />
            </Field>
          </Show>
          <Field label="Username">
            <Input
              autofocus={!props.setupCode}
              required
              autocomplete="username"
              value={username()}
              onValue={setUsername}
            />
          </Field>
          <Field label="Password" hint={props.newPassword ? "At least 10 characters." : undefined}>
            <Input
              type="password"
              required
              minLength={props.newPassword ? 10 : undefined}
              autocomplete={props.newPassword ? "new-password" : "current-password"}
              value={password()}
              onValue={setPassword}
            />
          </Field>
          <Show when={error()}>
            <Note tone="bad">{error()}</Note>
          </Show>
          <Button type="submit" variant="primary" block disabled={busy()} style={{ "margin-top": "4px" }}>
            {props.submit}
          </Button>
        </form>
      </div>
    </div>
  );
}
