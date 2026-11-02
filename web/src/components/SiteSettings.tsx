import { createMemo, createSignal, Show, untrack } from "solid-js";
import { api, type DeploymentDetail, type Spec } from "../api";
import { type EnvRow, envToRows, rowsToEnv, type SiteDraft, siteDraft, siteOf, withEnvRefs } from "../spec";
import useToastState, { errorMessage } from "../states/toastState";
import { SiteFields } from "./SiteFields";
import { Card, Note } from "./ui/Blocks";
import Button from "./ui/Button/Button";
import { EnvRows } from "./ui/Editors";
import Field from "./ui/Field/Field";
import { Icon } from "./ui/Icon";
import { SaveBar, same } from "./ui/Overlay";

interface Draft {
  site: SiteDraft;
  env: EnvRow[];
}

function draftOf(d: DeploymentDetail): Draft {
  return {
    site: siteDraft(d.spec.site),
    env: envToRows(d.spec.container.env, d.spec.secrets ?? [], d.spec.secret_refs),
  };
}

/** Settings of a static site: how it's built, its build environment, and new files for uploaded sites. */
export function SiteSettings(props: { detail: DeploymentDetail; onSaved: (spec: Spec) => void }) {
  const { pushToast, pushError } = useToastState;
  const baseline = createMemo(() => draftOf(props.detail));
  const [draft, setDraft] = createSignal<Draft>(untrack(baseline));
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  let fileInput!: HTMLInputElement;

  const uploaded = () => props.detail.spec.source.kind === "static";
  const canDeploy = () => props.detail.permissions.includes("dep.deploy");

  async function save() {
    setError("");
    setBusy(true);
    try {
      const { env, secrets, refs } = rowsToEnv(draft().env);
      const spec: Spec = {
        ...props.detail.spec,
        container: { ...props.detail.spec.container, env },
        secrets,
        secret_refs: withEnvRefs(props.detail.spec, refs),
        site: siteOf(draft().site),
      };
      await api.updateDeployment(props.detail.id, spec);
      pushToast("Site settings saved. They apply on the next deploy.");
      props.onSaved(spec);
      setDraft(draftOf({ ...props.detail, spec }));
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  async function upload(file: File | undefined) {
    if (!file) return;
    setBusy(true);
    try {
      await api.uploadSite(props.detail.id, file, true);
      pushToast(`Uploaded ${file.name}. Deploying it now.`);
    } catch (err) {
      pushError(err);
    } finally {
      setBusy(false);
      fileInput.value = "";
    }
  }

  return (
    <div class="stack">
      <Show when={uploaded() && canDeploy()}>
        <Card
          title="Files"
          description="Upload a new archive of the site, its source or the finished files. It deploys right away."
          actions={
            <Button size="sm" variant="primary" disabled={busy()} onClick={() => fileInput.click()}>
              <Icon name="upload" />
              Upload new files
            </Button>
          }
        />
        <input
          ref={fileInput}
          class="hidden"
          type="file"
          accept=".tar,.tgz,.tar.gz,.tar.bz2,.tar.xz,application/x-tar,application/gzip"
          onChange={(e) => upload(e.currentTarget.files?.[0])}
        />
      </Show>
      <Card title="Site" description="How the site is built and which folder is served">
        <div class="card-b">
          <SiteFields draft={draft().site} onChange={(site) => setDraft({ ...draft(), site })} />
        </div>
      </Card>
      <Card title="Build environment" description="Variables passed to the build command">
        <div class="card-b">
          <Field label="Environment" hint="Use the lock to mark a value secret.">
            <EnvRows
              rows={draft().env}
              names={props.detail.secret_names}
              onChange={(env) => setDraft({ ...draft(), env })}
            />
          </Field>
        </div>
      </Card>
      <Show when={error()}>
        <Note tone="bad">{error()}</Note>
      </Show>
      <SaveBar
        show={!same(draft(), baseline())}
        busy={busy()}
        message="Site settings changed."
        onSave={save}
        onDiscard={() => setDraft(baseline())}
      />
    </div>
  );
}
