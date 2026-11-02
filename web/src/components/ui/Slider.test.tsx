import { fireEvent, render } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { describe, expect, it } from "vitest";
import { Slider } from "./Form";

function setup(fallback: number | null, initial: number | null = null) {
  const [value, setValue] = createSignal<number | null>(initial);
  const view = render(() => (
    <Slider
      label="Memory"
      value={value()}
      onChange={setValue}
      min={64}
      max={4096}
      step={64}
      unit="MB"
      fallback={fallback}
    />
  ));
  const range = view.getByLabelText("Memory") as HTMLInputElement;
  const box = view.getByLabelText("Memory value") as HTMLInputElement;
  return { value, range, box };
}

describe("Slider", () => {
  it("sits at the default and shows it as the placeholder when empty", () => {
    const { range, box } = setup(512);
    expect(range.value).toBe("512");
    expect(range.classList.contains("is-def")).toBe(true);
    expect(box.placeholder).toBe("512");
    expect(box.value).toBe("");
  });

  it("typing a number sets it, clearing the box goes back to the default", () => {
    const { value, box } = setup(512);
    fireEvent.input(box, { target: { value: "1024" } });
    expect(value()).toBe(1024);
    fireEvent.input(box, { target: { value: "" } });
    expect(value()).toBe(null);
  });

  it("dragging fills the box", () => {
    const { value, range, box } = setup(512);
    fireEvent.input(range, { target: { value: "2048" } });
    expect(value()).toBe(2048);
    expect(box.value).toBe("2048");
  });

  it("without a default, the far right means no limit", () => {
    const { value, range, box } = setup(null, 1024);
    expect(box.placeholder).toBe("no limit");
    fireEvent.input(range, { target: { value: "4096" } });
    expect(value()).toBe(null);
    expect(range.value).toBe("4096");
  });
});
