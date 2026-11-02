import { A, useNavigate } from "@solidjs/router";
import { createResource, createSignal, For, Show } from "solid-js";
import { useUser } from "../App";
import { api, can, type Deployment, type SourceKind } from "../api";
import { Card, Note, PageHeader, StatusBadge } from "../components/ui/Blocks";
import Button from "../components/ui/Button/Button";
import EmptyState from "../components/ui/EmptyState/EmptyState";
import { Icon, type IconName } from "../components/ui/Icon";
import Input from "../components/ui/Input/Input";
import { ready } from "../components/ui/Overlay";
import SegmentedControl from "../components/ui/SegmentedControl/SegmentedControl";
import { Bone, Busy, SkeletonEmpty, SkeletonTable } from "../components/ui/Skeleton";
import { formatAgo, formatTime } from "../format";
import { useLive } from "../live";
import { rowsFor, useShape } from "../shape";
import { sourceLabel } from "../spec";
import { EVENT_DEBOUNCE_MS, eventsConnected, isDeploymentEvent, onEvent } from "../states/eventState";

type Filter = "all" | "running" | "issues" | "stopped";

export const KIND_ICON: Record<SourceKind, IconName> = {
  image: "layers",
  git: "git",
  upload: "upload",
  compose: "code",
  dockerfile: "file",
  static: "globe",
};
export const KIND_LABEL: Record<SourceKind, string> = {
  image: "Image",
  git: "Git",
  upload: "Upload",
  compose: "Compose",
  dockerfile: "Dockerfile",
  static: "Static site",
};

const needsLook = (d: Deployment) => ["failed", "down", "degraded", "deploying"].includes(d.status);
const FILTERS: Record<Filter, (d: Deployment) => boolean> = {
  all: () => true,
  running: (d) => d.status === "running",
  issues: needsLook,
  stopped: (d) => d.status === "stopped" || d.status === "created",
};

export function Deployments() {
  const user = useUser();
  const navigate = useNavigate();
  const [deployments, { refetch }] = createResource(api.deployments);
  onEvent(isDeploymentEvent, refetch, EVENT_DEBOUNCE_MS);
  useLive(() => !eventsConnected() && refetch());
  const [filter, setFilter] = createSignal<Filter>("all");
  const [search, setSearch] = createSignal("");

  const all = () => ready(deployments) ?? [];
  const known = useShape("deployments", 3, () => (deployments.latest ? all().length : undefined));
  const rows = () => {
    const needle = search().trim().toLowerCase();
    return all().filter(
      (d) =>
        FILTERS[filter()](d) &&
        (!needle || d.name.toLowerCase().includes(needle) || sourceLabel(d.spec).toLowerCase().includes(needle)),
    );
  };
  const count = (f: Filter) => all().filter(FILTERS[f]).length;
  const canCreate = () => can(user(), "dep.create");

  const createButton = (
    <Button variant="primary" href="/deployments/new">
      <Icon name="plus" />
      New deployment
    </Button>
  );

  return (
    <div class="content">
      <PageHeader
        title="Deployments"
        description="Everything running on this server"
        actions={<Show when={canCreate()}>{createButton}</Show>}
      />
      <Show when={!deployments.error} fallback={<Note tone="bad">{String(deployments.error?.message)}</Note>}>
        <Show
          when={!deployments.loading || all().length > 0}
          fallback={
            <Busy>
              <Show
                when={known > 0}
                fallback={
                  <section class="card">
                    <SkeletonEmpty action={canCreate()} />
                  </section>
                }
              >
                <div class="hrow wrap" style={{ "margin-bottom": "14px", gap: "10px" }}>
                  <Bone w="340px" size="control" />
                  <span class="grow" />
                  <Bone w="380px" size="control" />
                </div>
                <SkeletonTable rows={rowsFor(known)} columns={5} />
              </Show>
            </Busy>
          }
        >
          <Show
            when={all().length > 0}
            fallback={
              <Card>
                <EmptyState
                  icon="box"
                  title="No deployments yet"
                  description="Deploy an image, a Git repository, an uploaded build context or a compose file."
                  actions={<Show when={canCreate()}>{createButton}</Show>}
                />
              </Card>
            }
          >
            <div class="hrow wrap" style={{ "margin-bottom": "14px", gap: "10px" }}>
              <div class="grow" style={{ "max-width": "340px" }}>
                <Input
                  type="search"
                  clearable
                  before={<Icon name="search" />}
                  label="Search deployments"
                  placeholder="Search by name or source"
                  value={search()}
                  onValue={setSearch}
                />
              </div>
              <span class="grow" />
              <SegmentedControl
                label="Filter by status"
                value={filter()}
                onChange={setFilter}
                options={[
                  { value: "all", label: "All", count: count("all") },
                  { value: "running", label: "Running", count: count("running") },
                  { value: "issues", label: "Needs a look", count: count("issues") },
                  { value: "stopped", label: "Stopped", count: count("stopped") },
                ]}
              />
            </div>
            <Card>
              <Show
                when={rows().length > 0}
                fallback={
                  <EmptyState
                    icon="search"
                    title="Nothing matches"
                    description="Try another filter or search."
                    actions={
                      <Button
                        size="sm"
                        onClick={() => {
                          setFilter("all");
                          setSearch("");
                        }}
                      >
                        Show all
                      </Button>
                    }
                  />
                }
              >
                <table class="tbl">
                  <thead>
                    <tr>
                      <th>Name</th>
                      <th>Status</th>
                      <th class="hide-sm">Source</th>
                      <th class="num">Revision</th>
                      <th class="hide-sm">Updated</th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={rows()}>
                      {(d) => (
                        <tr class="click" onClick={() => navigate(`/deployments/${d.id}`)}>
                          <td>
                            <div class="nm">
                              <A href={`/deployments/${d.id}`} onClick={(e) => e.stopPropagation()}>
                                <b>{d.name}</b>
                              </A>
                              <span>{sourceLabel(d.spec)}</span>
                            </div>
                          </td>
                          <td>
                            <StatusBadge status={d.status} />
                          </td>
                          <td class="hide-sm">
                            <span class="src">
                              <Icon name={KIND_ICON[d.spec.source.kind]} />
                              {KIND_LABEL[d.spec.source.kind]}
                            </span>
                          </td>
                          <td class="num">{d.revision}</td>
                          <td class="hide-sm muted" title={`${formatTime(d.updated_at)} UTC`}>
                            {formatAgo(d.updated_at)}
                          </td>
                        </tr>
                      )}
                    </For>
                  </tbody>
                </table>
              </Show>
            </Card>
          </Show>
        </Show>
      </Show>
    </div>
  );
}
