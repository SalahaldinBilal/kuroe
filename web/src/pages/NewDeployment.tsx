import { A, useNavigate } from "@solidjs/router";
import { Switch as Branch, createResource, createSignal, Match, Show } from "solid-js";
import { useUser } from "../App";
import { api, can, GIT_TOKEN_REF, type RunMode, type Source, type SourceKind } from "../api";
import { SecretValue } from "../components/SecretValue";
import { SiteFields } from "../components/SiteFields";
import { Card, Note, PageHeader } from "../components/ui/Blocks";
import Button from "../components/ui/Button/Button";
import { EnvRows, PortRows } from "../components/ui/Editors";
import Field, { FieldRow } from "../components/ui/Field/Field";
import { Tiles } from "../components/ui/Form";
import { Icon } from "../components/ui/Icon";
import Input from "../components/ui/Input/Input";
import { ready } from "../components/ui/Overlay";
import SegmentedControl from "../components/ui/SegmentedControl/SegmentedControl";
import Select from "../components/ui/Select/Select";
import Switch from "../components/ui/Switch/Switch";
import { clonesGit, KIND_LABEL, pullsImages } from "../identities";
import {
  type EnvRow,
  emptyEnv,
  emptyPort,
  NODE_SITE,
  type PortRow,
  rowsToEnv,
  rowsToPorts,
  type SiteDraft,
  siteOf,
} from "../spec";
import useToastState, { errorMessage } from "../states/toastState";

const NAME_PATTERN = "[a-z0-9][a-z0-9\\-]{0,39}";
const IMAGE_SITE_FOLDER = "/usr/share/nginx/html";

type SiteFrom = "git" | "image" | "upload";

function LoginsHint() {
  return (
    <>
      Saved logins live under{" "}
      <A class="link" href="/identities">
        Identities
      </A>
      .
    </>
  );
}

export function NewDeployment() {
  const navigate = useNavigate();
  const user = useUser();
  const { pushToast } = useToastState;
  const [info] = createResource(api.dockerInfo);
  const canUseLogins = () => can(user(), "ident.use") || can(user(), "ident.manage");
  const [logins] = createResource(() => canUseLogins() || null, api.identities);
  const [access, setAccess] = createSignal("public");
  const [registryLogin, setRegistryLogin] = createSignal("");
  const appLogin = () => (ready(logins) ?? []).find((i) => i.id === access() && i.kind === "github_app");
  const [repos] = createResource(() => appLogin()?.id, api.identityRepos);
  const gitLogins = () => (ready(logins) ?? []).filter((i) => clonesGit(i.kind));
  const imageLogins = () => (ready(logins) ?? []).filter((i) => pullsImages(i.kind));
  const accessItems = () => [
    { value: "public", label: "Public repository" },
    ...gitLogins().map((i) => ({ value: i.id, label: `${i.name} (${KIND_LABEL[i.kind]})` })),
    { value: "token", label: "Paste a token for this repository" },
  ];
  const registryItems = () => [
    { value: "", label: "None, public image" },
    ...imageLogins().map((i) => ({ value: i.id, label: `${i.name} (${i.host || KIND_LABEL[i.kind]})` })),
  ];
  const [kind, setKind] = createSignal<SourceKind>("image");
  const [siteFrom, setSiteFrom] = createSignal<SiteFrom>("git");
  const [name, setName] = createSignal("");
  const [image, setImage] = createSignal("");
  const [git, setGit] = createSignal({ url: "", reference: "", context: "", dockerfile: "", token: "", compose: "" });
  const [tokenRef, setTokenRef] = createSignal<string | null>(null);
  const [buildWith, setBuildWith] = createSignal<"dockerfile" | "compose">("dockerfile");
  const composeLike = () => kind() === "compose" || (kind() === "git" && buildWith() === "compose");
  const staticLike = () => kind() === "static";
  const usesGit = () => kind() === "git" || (staticLike() && siteFrom() === "git");
  const [site, setSite] = createSignal<SiteDraft>({ ...NODE_SITE });
  const needsFile = () => kind() === "upload" || (staticLike() && siteFrom() === "upload");
  const [dockerfile, setDockerfile] = createSignal("");
  const [dockerfileText, setDockerfileText] = createSignal("");
  const [archive, setArchive] = createSignal<File | null>(null);
  const [compose, setCompose] = createSignal("");
  const [ports, setPorts] = createSignal<PortRow[]>([emptyPort()]);
  const [env, setEnv] = createSignal<EnvRow[]>([emptyEnv()]);
  const [mode, setMode] = createSignal<RunMode>("compose");
  const [deployNow, setDeployNow] = createSignal(true);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [dragging, setDragging] = createSignal(false);
  let fileInput!: HTMLInputElement;
  let dockerfileInput!: HTMLInputElement;

  const optional = (value: string) => value.trim() || null;
  const setGitField = (key: keyof ReturnType<typeof git>) => (value: string) => setGit({ ...git(), [key]: value });
  const pastesToken = () => access() === "token" || !canUseLogins();

  function pickSiteFrom(from: SiteFrom) {
    setSiteFrom(from);
    if (from === "image") setSite({ ...site(), build: "none", output: IMAGE_SITE_FOLDER });
    else if (site().output === IMAGE_SITE_FOLDER) setSite({ ...NODE_SITE });
  }

  function gitSource(siteMode: boolean): Source {
    const g = git();
    const saved = access() !== "public" && access() !== "token" ? access() : null;
    const compose = !siteMode && buildWith() === "compose";
    if (compose && !g.compose.trim()) throw new Error("Give the path of the Compose file in the repository");
    return {
      kind: "git",
      url: g.url.trim(),
      reference: optional(g.reference),
      context: compose ? null : optional(g.context),
      dockerfile: !siteMode && buildWith() === "dockerfile" ? optional(g.dockerfile) : null,
      token: pastesToken() && !tokenRef() ? optional(g.token) : null,
      identity: saved,
      compose_file: compose ? g.compose.trim() : null,
    };
  }

  function imageSource(): Source {
    return { kind: "image", image: image().trim(), registry_identity: registryLogin() || null };
  }

  function source(): Source {
    switch (kind()) {
      case "image":
        return imageSource();
      case "git":
        return gitSource(false);
      case "upload":
        return { kind: "upload", dockerfile: optional(dockerfile()) };
      case "compose":
        return { kind: "compose", file: compose() };
      case "dockerfile":
        return { kind: "dockerfile", file: dockerfileText() };
      case "static":
        if (siteFrom() === "git") return gitSource(true);
        if (siteFrom() === "image") return imageSource();
        return { kind: "static" };
    }
  }

  async function loadDockerfile(file: File | undefined) {
    if (file) setDockerfileText(await file.text());
    dockerfileInput.value = "";
  }

  async function onSubmit(event: SubmitEvent) {
    event.preventDefault();
    setError("");
    setBusy(true);
    try {
      const single = !composeLike();
      const { env: envMap, secrets, refs } = rowsToEnv(env());
      const token = usesGit() && pastesToken() ? tokenRef() : null;
      const spec = {
        source: source(),
        container: { ports: single && !staticLike() ? rowsToPorts(ports()) : [], env: envMap },
        services: {},
        secrets,
        secret_refs: token ? { ...refs, [GIT_TOKEN_REF]: token } : refs,
        mode: staticLike() ? ("compose" as const) : mode(),
        site: staticLike() ? siteOf(siteFrom() === "image" ? { ...site(), build: "none" } : site()) : null,
      };
      const created = await api.createDeployment(name().trim(), spec);
      const file = archive();
      if (kind() === "upload" && file) await api.uploadContext(created.id, file);
      if (staticLike() && siteFrom() === "upload" && file) await api.uploadSite(created.id, file);
      if (deployNow() && (!needsFile() || file)) {
        await api.deploy(created.id);
        pushToast(`Created ${created.name}. Deploying revision 1.`);
      } else {
        pushToast(`Created ${created.name}`);
      }
      navigate(`/deployments/${created.id}`);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  function pickFile(file: File | undefined) {
    if (file) setArchive(file);
  }

  function dropZone(what: string) {
    return (
      <>
        <button
          type="button"
          class={dragging() ? "drop over" : "drop"}
          onClick={() => fileInput.click()}
          onDragOver={(e) => {
            e.preventDefault();
            setDragging(true);
          }}
          onDragLeave={() => setDragging(false)}
          onDrop={(e) => {
            e.preventDefault();
            setDragging(false);
            pickFile(e.dataTransfer?.files[0]);
          }}
        >
          <Icon name="upload" />
          <Show
            when={archive()}
            fallback={
              <>
                <b>Drop {what} here</b>
                <span>.tar, .tar.gz, .tar.bz2 or .tar.xz, or click to choose a file</span>
              </>
            }
          >
            {(file) => (
              <>
                <b class="mono">{file().name}</b>
                <span>Click to choose another file</span>
              </>
            )}
          </Show>
        </button>
        <input
          ref={fileInput}
          class="hidden"
          type="file"
          accept=".tar,.tgz,.tar.gz,.tar.bz2,.tar.xz,application/x-tar,application/gzip"
          onChange={(e) => pickFile(e.currentTarget.files?.[0])}
        />
      </>
    );
  }

  function branchField() {
    return (
      <Field label="Branch or tag">
        <Input mono placeholder="default branch" value={git().reference} onValue={setGitField("reference")} />
      </Field>
    );
  }

  function imageFields() {
    return (
      <>
        <Field label="Image" hint="Registry, name and tag.">
          <Input mono required label="Image" placeholder="ghcr.io/acme/api:1.4" value={image()} onValue={setImage} />
        </Field>
        <Show when={canUseLogins()}>
          <Field label="Registry login" hint={<LoginsHint />}>
            <Select
              label="Registry login"
              items={registryItems()}
              value={registryLogin()}
              onChange={setRegistryLogin}
            />
          </Field>
        </Show>
      </>
    );
  }

  function repositoryFields() {
    return (
      <>
        <Show when={canUseLogins()}>
          <Field label="Access" hint={<LoginsHint />}>
            <Select label="Access" items={accessItems()} value={access()} onChange={setAccess} />
          </Field>
        </Show>
        <Show
          when={appLogin()}
          fallback={
            <Field label="Repository URL">
              <Input
                mono
                required
                label="Repository URL"
                placeholder="https://github.com/acme/api.git"
                value={git().url}
                onValue={setGitField("url")}
              />
            </Field>
          }
        >
          <Field
            label="Repository"
            hint={
              repos.error
                ? errorMessage(repos.error)
                : "Repositories the GitHub App can read. Pushes deploy through the app."
            }
          >
            <Select
              mono
              label="Repository"
              placeholder={repos.loading ? "Loading repositories" : "Pick a repository"}
              items={(ready(repos) ?? []).map((r) => ({ value: r.clone_url, label: r.full_name }))}
              value={git().url}
              onChange={setGitField("url")}
            />
          </Field>
        </Show>
        <Show when={pastesToken()}>
          <Field label="Access token" hint="A raw token stays on this deployment. Use a saved secret to share one.">
            <SecretValue
              label="Access token"
              value={git().token}
              secretRef={tokenRef()}
              secret
              placeholder="token"
              onChange={(next) => {
                setGitField("token")(next.value);
                setTokenRef(next.ref);
              }}
            />
          </Field>
        </Show>
      </>
    );
  }

  return (
    <div class="content narrow">
      <div class="crumbs">
        <A href="/deployments">Deployments</A>
        <Icon name="right" />
        <span>New</span>
      </div>
      <PageHeader title="New deployment" />
      <form class="stack" onSubmit={onSubmit}>
        <Card title="Basics">
          <div class="card-b">
            <Field label="Name" hint="Lowercase letters, numbers and dashes. Up to 40 characters.">
              <Input
                mono
                required
                autofocus
                label="Name"
                pattern={NAME_PATTERN}
                placeholder="shop-api"
                value={name()}
                onValue={setName}
              />
            </Field>
          </div>
        </Card>

        <Card title="Source" description="Where the code or image comes from">
          <div class="card-b stack">
            <Tiles
              label="Source"
              columns={3}
              value={kind()}
              onChange={setKind}
              options={[
                { value: "image", label: "Image", icon: "layers", description: "Run an image from a registry." },
                {
                  value: "git",
                  label: "Git repository",
                  icon: "git",
                  description: "Build its Dockerfile or run its Compose file.",
                },
                {
                  value: "static",
                  label: "Static site",
                  icon: "globe",
                  description: "Serve files from a repository, an image or an archive, built here or ready.",
                },
                {
                  value: "dockerfile",
                  label: "Dockerfile",
                  icon: "file",
                  description: "Write or load a Dockerfile and build it here.",
                },
                { value: "upload", label: "Upload", icon: "upload", description: "Build from a .tar build context." },
                {
                  value: "compose",
                  label: "Compose file",
                  icon: "code",
                  description: "Several services from compose.yaml.",
                },
              ]}
            />
            <Branch>
              <Match when={kind() === "image"}>
                <div class="stack" style={{ gap: "14px" }}>
                  {imageFields()}
                </div>
              </Match>
              <Match when={kind() === "git"}>
                <div class="stack" style={{ gap: "14px" }}>
                  {repositoryFields()}
                  <Field label="Build with">
                    <div>
                      <SegmentedControl
                        label="Build with"
                        value={buildWith()}
                        onChange={setBuildWith}
                        options={[
                          { value: "dockerfile" as const, label: "Dockerfile" },
                          { value: "compose" as const, label: "Compose file" },
                        ]}
                      />
                    </div>
                  </Field>
                  <Show when={buildWith() === "compose"}>
                    <FieldRow>
                      {branchField()}
                      <Field
                        label="Compose file"
                        grow
                        hint="Path inside the repository. Its relative paths and its .env file resolve from the folder it's in."
                      >
                        <Input
                          mono
                          label="Compose file"
                          placeholder="compose.yaml"
                          value={git().compose}
                          onValue={setGitField("compose")}
                        />
                      </Field>
                    </FieldRow>
                  </Show>
                  <Show when={buildWith() === "dockerfile"}>
                    <FieldRow>
                      {branchField()}
                      <Field label="Context folder">
                        <Input
                          mono
                          placeholder="repository root"
                          value={git().context}
                          onValue={setGitField("context")}
                        />
                      </Field>
                      <Field label="Dockerfile">
                        <Input
                          mono
                          placeholder="Dockerfile"
                          value={git().dockerfile}
                          onValue={setGitField("dockerfile")}
                        />
                      </Field>
                    </FieldRow>
                  </Show>
                </div>
              </Match>
              <Match when={kind() === "static"}>
                <div class="stack" style={{ gap: "14px" }}>
                  <Field label="Files come from">
                    <div>
                      <SegmentedControl
                        label="Files come from"
                        value={siteFrom()}
                        onChange={pickSiteFrom}
                        options={[
                          { value: "git" as const, label: "Git repository" },
                          { value: "image" as const, label: "Image" },
                          { value: "upload" as const, label: "Upload" },
                        ]}
                      />
                    </div>
                  </Field>
                  <Branch>
                    <Match when={siteFrom() === "git"}>
                      {repositoryFields()}
                      <FieldRow>
                        {branchField()}
                        <Field label="Source folder" hint="Where the site lives in the repository.">
                          <Input
                            mono
                            placeholder="repository root"
                            value={git().context}
                            onValue={setGitField("context")}
                          />
                        </Field>
                      </FieldRow>
                    </Match>
                    <Match when={siteFrom() === "image"}>{imageFields()}</Match>
                    <Match when={siteFrom() === "upload"}>
                      {dropZone("the site's source or files")}
                      <span class="hint">
                        You can also leave this for now and let CI upload archives with a deploy token.
                      </span>
                    </Match>
                  </Branch>
                  <SiteFields draft={site()} onChange={setSite} fromImage={siteFrom() === "image"} />
                </div>
              </Match>
              <Match when={kind() === "dockerfile"}>
                <Field
                  label="Dockerfile"
                  hint="It's built on its own, with no other files next to it. Use a git repository or an upload when it copies in your code."
                >
                  <div class="stack" style={{ gap: "8px" }}>
                    <textarea
                      class="ta"
                      rows={14}
                      required
                      aria-label="Dockerfile"
                      placeholder={"FROM nginx:alpine\nRUN echo 'hello' > /usr/share/nginx/html/index.html"}
                      value={dockerfileText()}
                      onInput={(e) => setDockerfileText(e.currentTarget.value)}
                    />
                    <div class="hrow">
                      <Button size="sm" onClick={() => dockerfileInput.click()}>
                        <Icon name="upload" />
                        Load from a file
                      </Button>
                    </div>
                    <input
                      ref={dockerfileInput}
                      class="hidden"
                      type="file"
                      onChange={(e) => loadDockerfile(e.currentTarget.files?.[0])}
                    />
                  </div>
                </Field>
              </Match>
              <Match when={kind() === "upload"}>
                <div class="stack" style={{ gap: "14px" }}>
                  {dropZone("the build context")}
                  <Field label="Dockerfile" hint="Path inside the archive.">
                    <Input mono placeholder="Dockerfile" value={dockerfile()} onValue={setDockerfile} />
                  </Field>
                </div>
              </Match>
              <Match when={kind() === "compose"}>
                <Field
                  label="compose.yaml"
                  hint="Ports bind to 127.0.0.1 unless you mark the service public in its settings after creating it."
                >
                  <textarea
                    class="ta"
                    rows={14}
                    required
                    aria-label="compose.yaml"
                    placeholder={"services:\n  web:\n    image: nginx:alpine\n    ports:\n      - 3000:80"}
                    value={compose()}
                    onInput={(e) => setCompose(e.currentTarget.value)}
                  />
                </Field>
              </Match>
            </Branch>
          </div>
        </Card>

        <Show when={!composeLike() || ready(info)?.swarm_manager}>
          <Card
            title={staticLike() ? "Build environment" : "Container"}
            description="You can change these later in the deployment settings"
          >
            <div class="card-b stack" style={{ gap: "20px" }}>
              <Show when={!composeLike() && !staticLike()}>
                <Field label="Ports" hint="Host ports bind to 127.0.0.1.">
                  <PortRows rows={ports()} onChange={setPorts} />
                </Field>
              </Show>
              <Show when={!composeLike() && !(staticLike() && siteFrom() === "image")}>
                <Field
                  label="Environment"
                  hint={
                    staticLike()
                      ? "Passed to the build command, like VITE_API_URL. Secrets are hidden from viewers and left out of exports."
                      : "Use the lock to mark a value secret. Secrets are hidden from viewers and left out of exports."
                  }
                >
                  <EnvRows rows={env()} onChange={setEnv} />
                </Field>
              </Show>
              <Show when={staticLike() && siteFrom() === "image"}>
                <span class="hint">The image's files are served as they are, so there's nothing to set here.</span>
              </Show>
              <Show when={ready(info)?.swarm_manager && !staticLike()}>
                <Field label="Run as" hint="A Swarm stack gives rolling updates and replicas across nodes.">
                  <div>
                    <SegmentedControl
                      label="Run as"
                      value={mode()}
                      onChange={setMode}
                      options={[
                        { value: "compose" as const, label: "Compose project" },
                        { value: "stack" as const, label: "Swarm stack" },
                      ]}
                    />
                  </div>
                </Field>
              </Show>
            </div>
          </Card>
        </Show>

        <Show when={error()}>
          <Note tone="bad">{error()}</Note>
        </Show>

        <Card>
          <div class="footbar">
            <Switch
              label={needsFile() && !archive() ? "Deploy right away (needs a file)" : "Deploy right away"}
              checked={deployNow()}
              onChange={setDeployNow}
            />
            <span class="grow" />
            <Button variant="ghost" href="/deployments">
              Cancel
            </Button>
            <Button type="submit" variant="primary" disabled={busy()}>
              Create deployment
            </Button>
          </div>
        </Card>
      </form>
    </div>
  );
}
