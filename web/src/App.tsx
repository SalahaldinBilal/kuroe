import { A, Route, Router, type RouteSectionProps, useLocation, useNavigate } from "@solidjs/router";
import { createContext, createEffect, createSignal, For, Match, onMount, Show, Switch, useContext } from "solid-js";
import { ApiError, api, can, isOwner, type User } from "./api";
import { ChangePassword } from "./components/ChangePassword";
import { HttpBanner } from "./components/HttpAccess";
import { StartupFailed } from "./components/StartupFailed";
import Button from "./components/ui/Button/Button";
import ContextMenuHost from "./components/ui/ContextMenu/ContextMenuHost";
import { Icon, type IconName } from "./components/ui/Icon";
import { Logo } from "./components/ui/Logo";
import { SkeletonShell } from "./components/ui/Skeleton";
import Toasts from "./components/ui/Toasts/Toasts";
import TooltipHost from "./components/ui/Tooltip/TooltipHost";
import { Credentials } from "./pages/Credentials";
import { Dashboard } from "./pages/Dashboard";
import { Deployment } from "./pages/Deployment";
import { Deployments } from "./pages/Deployments";
import { Identities } from "./pages/Identities";
import { Import } from "./pages/Import";
import { Metrics } from "./pages/Metrics";
import { NewDeployment } from "./pages/NewDeployment";
import { ONBOARDING_DONE, Onboarding } from "./pages/Onboarding";
import { Preview } from "./pages/Preview";
import { ProxyPage } from "./pages/Proxy";
import { Runners } from "./pages/Runners";
import { Secrets } from "./pages/Secrets";
import { Settings } from "./pages/Settings";
import { Users } from "./pages/Users";
import { Volumes } from "./pages/Volumes";
import { startEvents, stopEvents } from "./states/eventState";

type Phase =
  | { kind: "loading" }
  | { kind: "setup" }
  | { kind: "login" }
  | { kind: "signed-in"; user: User }
  | { kind: "startup-failed"; message: string }
  | { kind: "error"; message: string };

const UserContext = createContext<() => User>();

export function useUser(): () => User {
  const user = useContext(UserContext);
  if (!user) throw new Error("useUser outside signed-in shell");
  return user;
}

export function App() {
  const [phase, setPhase] = createSignal<Phase>({ kind: "loading" });

  async function load() {
    try {
      const { needed } = await api.setupStatus();
      if (needed) return setPhase({ kind: "setup" });
      setPhase({ kind: "signed-in", user: await api.me() });
    } catch (err) {
      if (err instanceof ApiError && err.status === 401) return setPhase({ kind: "login" });
      if (err instanceof ApiError && err.status === 503)
        return setPhase({ kind: "startup-failed", message: err.message });
      setPhase({ kind: "error", message: String(err) });
    }
  }

  /** The sign-in response has no permissions, so the full user comes from /me. */
  async function signedIn() {
    try {
      setPhase({ kind: "signed-in", user: await api.me() });
    } catch (err) {
      setPhase({ kind: "error", message: String(err) });
    }
  }

  async function signOut() {
    await api.logout();
    setPhase({ kind: "login" });
  }

  onMount(load);

  createEffect(() => (phase().kind === "signed-in" ? startEvents() : stopEvents()));

  const signedInUser = () => {
    const p = phase();
    return p.kind === "signed-in" ? p.user : undefined;
  };

  return (
    <>
      <Screens />
      <Toasts />
      <TooltipHost />
      <ContextMenuHost />
    </>
  );

  function Screens() {
    return (
      <Switch>
        <Match when={phase().kind === "loading"}>
          <SkeletonShell />
        </Match>
        <Match when={phase().kind === "error"}>
          <div class="center error-text">{(phase() as { message: string }).message}</div>
        </Match>
        <Match when={phase().kind === "startup-failed"}>
          <StartupFailed message={(phase() as { message: string }).message} onRetry={load} />
        </Match>
        <Match when={phase().kind === "setup"}>
          <Credentials
            title="Create the owner account"
            hint="This runs once. The owner can add other users later."
            submit="Create owner"
            newPassword
            setupCode
            action={api.setup}
            onDone={signedIn}
          />
        </Match>
        <Match when={phase().kind === "login"}>
          <Credentials
            title="Sign in"
            hint="Use the account your server owner gave you."
            submit="Sign in"
            action={api.login}
            onDone={signedIn}
          />
        </Match>
        <Match when={signedInUser()}>
          {(user) => (
            <UserContext.Provider value={user}>
              <Router explicitLinks root={(props) => <Shell {...props} onSignOut={signOut} />}>
                <Route path="/" component={Dashboard} />
                <Route path="/deployments" component={Deployments} />
                <Route path="/deployments/new" component={NewDeployment} />
                <Route path="/deployments/:id" component={Deployment} />
                <Route path="/deployments/:id/preview" component={Preview} />
                <Route path="/volumes" component={Volumes} />
                <Route path="/metrics" component={Metrics} />
                <Route path="/import" component={Import} />
                <Route path="/proxy" component={ProxyPage} />
                <Route path="/identities" component={Identities} />
                <Route path="/secrets" component={Secrets} />
                <Route path="/runners" component={Runners} />
                <Route path="/settings" component={Settings} />
                <Route path="/users" component={Users} />
                <Route path="/onboarding" component={Onboarding} />
              </Router>
            </UserContext.Provider>
          )}
        </Match>
      </Switch>
    );
  }
}

interface NavItem {
  href: string;
  label: string;
  icon: IconName;
  end?: boolean;
  show?: (user: User) => boolean;
}

const MAIN_NAV: NavItem[] = [
  { href: "/", label: "Dashboard", icon: "grid", end: true },
  { href: "/deployments", label: "Deployments", icon: "box" },
  { href: "/metrics", label: "Metrics", icon: "activity" },
  { href: "/volumes", label: "Volumes", icon: "drive" },
];

const SERVER_NAV: NavItem[] = [
  { href: "/proxy", label: "Proxy", icon: "globe" },
  { href: "/identities", label: "Identities", icon: "key", show: (user) => can(user, "ident.manage") },
  {
    href: "/secrets",
    label: "Secrets",
    icon: "lock",
    show: (user) => can(user, "secret.use") || can(user, "secret.manage"),
  },
  { href: "/runners", label: "Runners", icon: "play", show: (user) => can(user, "runners") },
  { href: "/settings", label: "Settings", icon: "sliders" },
  { href: "/import", label: "Import", icon: "download", show: (user) => can(user, "import") },
  { href: "/users", label: "Users", icon: "users", show: (user) => can(user, "users") },
];

function Shell(props: RouteSectionProps & { onSignOut: () => void }) {
  const user = useUser();
  const location = useLocation();
  const navigate = useNavigate();
  const [changingPassword, setChangingPassword] = createSignal(false);

  onMount(async () => {
    if (!isOwner(user()) || location.pathname !== "/") return;
    const settings = await api.settings().catch(() => null);
    if (settings && settings[ONBOARDING_DONE] !== "true") navigate("/onboarding", { replace: true });
  });

  return (
    <Show when={location.pathname !== "/onboarding"} fallback={<main class="bare">{props.children}</main>}>
      <ShellFrame {...props} onPassword={() => setChangingPassword(true)} />
      <ChangePassword show={changingPassword()} onHide={() => setChangingPassword(false)} />
    </Show>
  );
}

function ShellFrame(props: RouteSectionProps & { onSignOut: () => void; onPassword: () => void }) {
  const user = useUser();
  const link = (item: NavItem) => (
    <Show when={!item.show || item.show(user())}>
      <A class="nav-i" activeClass="on" inactiveClass="" href={item.href} end={item.end}>
        <Icon name={item.icon} />
        <span>{item.label}</span>
      </A>
    </Show>
  );
  return (
    <div class="app">
      <aside class="side">
        <div class="brand">
          <Logo />
          kuroe
        </div>
        <nav class="nav">
          <For each={MAIN_NAV}>{link}</For>
          <div class="nav-label">Server</div>
          <For each={SERVER_NAV}>{link}</For>
        </nav>
        <div class="me">
          <span class="av">{user().username.slice(0, 2)}</span>
          <div class="me-t">
            <b>{user().username}</b>
            <span>{user().role}</span>
          </div>
          <Button variant="ghost" icon size="sm" tooltip="Change password" onClick={props.onPassword}>
            <Icon name="key" />
          </Button>
          <Button variant="ghost" icon size="sm" tooltip="Sign out" onClick={props.onSignOut}>
            <Icon name="logout" />
          </Button>
        </div>
      </aside>
      <main>
        <Show when={user().show_http_banner}>
          <HttpBanner />
        </Show>
        {props.children}
      </main>
    </div>
  );
}
