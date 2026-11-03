import { fireEvent, render } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { describe, expect, it } from "vitest";
import Select from "./Select";

const REPOS = ["acme/api", "acme/web", "tools/cli"].map((name) => ({ value: `https://x/${name}.git`, label: name }));

function setup() {
  const [value, setValue] = createSignal("");
  const view = render(() => <Select searchable label="Repository" items={REPOS} value={value()} onChange={setValue} />);
  const control = view.getByLabelText("Repository");
  fireEvent.click(control);
  const options = () => [...document.querySelectorAll("[role=option]")].map((o) => o.textContent);
  return { value, control, options };
}

describe("searchable Select", () => {
  it("filters as you type and picks the first match with Enter", () => {
    const { value, control, options } = setup();
    expect(options()).toEqual(["acme/api", "acme/web", "tools/cli"]);
    for (const key of "web") fireEvent.keyDown(control, { key });
    expect(options()).toEqual(["acme/web"]);
    fireEvent.keyDown(control, { key: "Enter" });
    expect(value()).toBe("https://x/acme/web.git");
  });

  it("Backspace widens the filter again and no match says so", () => {
    const { control, options } = setup();
    for (const key of "zz") fireEvent.keyDown(control, { key });
    expect(options()).toEqual([]);
    expect(document.body.textContent).toContain("No matches");
    fireEvent.keyDown(control, { key: "Backspace" });
    fireEvent.keyDown(control, { key: "Backspace" });
    expect(options()).toHaveLength(3);
  });
});
