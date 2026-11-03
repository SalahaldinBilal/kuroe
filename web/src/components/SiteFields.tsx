import { Match, Show, Switch } from "solid-js";
import type { SiteDraft } from "../spec";
import Field, { FieldRow } from "./ui/Field/Field";
import Input from "./ui/Input/Input";
import SegmentedControl from "./ui/SegmentedControl/SegmentedControl";
import Toggle from "./ui/Switch/Switch";

type Props = { draft: SiteDraft; onChange: (draft: SiteDraft) => void; disabled?: boolean };

function BuildStep(props: Props) {
  const set = (patch: Partial<SiteDraft>) => props.onChange({ ...props.draft, ...patch });
  return (
    <>
      <Field label="Build step">
        <SegmentedControl
          label="Build step"
          value={props.draft.build}
          onChange={(build) =>
            set({ build, output: build === "dockerfile" ? "/app/dist" : props.draft.output.replace(/^\//, "") })
          }
          options={
            [
              { value: "command", label: "Build command" },
              { value: "dockerfile", label: "Dockerfile" },
              { value: "none", label: "Files are ready" },
            ] as const
          }
        />
      </Field>
      <Switch>
        <Match when={props.draft.build === "command"}>
          <FieldRow>
            <Field label="Image" hint="Runs the command with the source in /src.">
              <Input
                mono
                label="Build image"
                disabled={props.disabled}
                placeholder="node:26-alpine"
                value={props.draft.image}
                onValue={(image) => set({ image })}
              />
            </Field>
            <Field label="Command" grow hint="Environment variables from the deployment are passed to it.">
              <Input
                mono
                label="Build command"
                disabled={props.disabled}
                placeholder="npm ci && npm run build"
                value={props.draft.command}
                onValue={(command) => set({ command })}
              />
            </Field>
          </FieldRow>
        </Match>
        <Match when={props.draft.build === "dockerfile"}>
          <Field label="Dockerfile" hint="Path in the source. The image is only built to copy the files out of it.">
            <Input
              mono
              label="Dockerfile"
              disabled={props.disabled}
              placeholder="Dockerfile"
              value={props.draft.dockerfile}
              onValue={(dockerfile) => set({ dockerfile })}
            />
          </Field>
        </Match>
      </Switch>
    </>
  );
}

/** How a static site is built and which folder gets served. An image is already built, so it only needs the folder. */
export function SiteFields(props: Props & { fromImage?: boolean }) {
  const set = (patch: Partial<SiteDraft>) => props.onChange({ ...props.draft, ...patch });
  const inImage = () => props.fromImage || props.draft.build === "dockerfile";
  return (
    <div class="stack" style={{ gap: "14px" }}>
      <Show when={!props.fromImage}>
        <BuildStep draft={props.draft} onChange={props.onChange} disabled={props.disabled} />
      </Show>
      <Field
        label={props.fromImage ? "Folder in the image" : "Output folder"}
        hint={
          props.fromImage
            ? "The folder in the image with the finished files. They're copied out and served, the image doesn't run."
            : inImage()
              ? "The folder in the built image with the finished files."
              : "The folder with the finished files, inside the source. Empty means the whole source."
        }
      >
        <Input
          mono
          label="Output folder"
          disabled={props.disabled}
          placeholder={props.fromImage ? "/usr/share/nginx/html" : inImage() ? "/app/dist" : "dist"}
          value={props.draft.output}
          onValue={(output) => set({ output })}
        />
      </Field>
      <Toggle
        label="Single-page app (answer unknown paths with index.html)"
        checked={props.draft.spa}
        disabled={props.disabled}
        onChange={(spa) => set({ spa })}
      />
    </div>
  );
}
