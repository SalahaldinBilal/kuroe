import { fireEvent, render } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { LineChart } from "./LineChart";

const MIB = 2 ** 20;
const GIB = 2 ** 30;

function memory(label: string, bytes: number) {
  return { label, points: [0, 10, 20].map((t) => ({ t, v: bytes })) };
}

function topTick(view: ReturnType<typeof render>): number {
  const ticks = [...view.container.querySelectorAll("text.nc-tick")].map((t) => Number(t.textContent));
  return Math.max(...ticks.filter((n) => !Number.isNaN(n)));
}

describe("hiding a line", () => {
  it("lets the small deployments fill the chart once the big one is hidden", () => {
    const view = render(() => (
      <LineChart
        title="Memory"
        binary
        format={(v) => String(v)}
        series={[memory("postgres", 2 * GIB), memory("api", 40 * MIB), memory("worker", 24 * MIB)]}
      />
    ));
    expect(topTick(view)).toBeGreaterThanOrEqual(2 * GIB);

    fireEvent.click(view.getByRole("button", { name: /postgres/ }));
    expect(topTick(view)).toBeGreaterThanOrEqual(40 * MIB);
    expect(topTick(view)).toBeLessThan(64 * MIB);
    expect(view.container.querySelector(".cc-v b")?.textContent).toBe(String(64 * MIB));

    fireEvent.click(view.getByRole("button", { name: /postgres/ }));
    expect(topTick(view)).toBeGreaterThanOrEqual(2 * GIB);
  });
});
