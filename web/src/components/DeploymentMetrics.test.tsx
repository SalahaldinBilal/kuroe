import { render } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { describe, expect, it, vi } from "vitest";

const calls = vi.hoisted(() => ({ metrics: 0 }));

vi.mock("../api", async (original) => {
  const actual = await original<typeof import("../api")>();
  return {
    ...actual,
    api: {
      ...actual.api,
      deploymentMetrics: async () => {
        calls.metrics++;
        return { bucket: 0, services: { web: [] }, containers: [] };
      },
      dockerInfo: async () => ({ cpus: 4, memory_bytes: 8 * 2 ** 30 }),
      settings: async () => ({}),
    },
  };
});

const { DeploymentMetrics } = await import("./DeploymentMetrics");

describe("deployment metrics", () => {
  it("don't reload when the deployment is refetched after a container starts or stops", async () => {
    const [detail, setDetail] = createSignal({ id: "dep-1", containers: [{ id: "c1", name: "shop-blue-web-1" }] });
    render(() => (
      <DeploymentMetrics deployment={detail().id} services={{}} stack={false} containers={detail().containers} />
    ));
    await vi.waitFor(() => expect(calls.metrics).toBe(1));

    setDetail({ id: "dep-1", containers: [{ id: "c2", name: "shop-green-web-1" }] });
    await new Promise((r) => setTimeout(r, 20));
    expect(calls.metrics).toBe(1);
  });
});
