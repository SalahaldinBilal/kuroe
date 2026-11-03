import { describe, expect, it } from "vitest";
import { copyKey } from "./terminalKeys";

const key = (change: Partial<KeyboardEvent>) =>
  ({
    type: "keydown",
    key: "c",
    ctrlKey: false,
    metaKey: false,
    shiftKey: false,
    altKey: false,
    ...change,
  }) as KeyboardEvent;

describe("terminal copy keys", () => {
  it("copies a selection with Ctrl+C or Cmd+C, and sends ^C without one", () => {
    expect(copyKey(key({ ctrlKey: true }), true)).toBe("copy");
    expect(copyKey(key({ metaKey: true }), true)).toBe("copy");
    expect(copyKey(key({ ctrlKey: true }), false)).toBe("pass");
  });

  it("always takes Ctrl+Shift+C, so the browser doesn't open its inspector", () => {
    expect(copyKey(key({ ctrlKey: true, shiftKey: true, key: "C" }), true)).toBe("copy");
    expect(copyKey(key({ ctrlKey: true, shiftKey: true, key: "C" }), false)).toBe("swallow");
  });

  it("leaves other keys alone", () => {
    expect(copyKey(key({}), true)).toBe("pass");
    expect(copyKey(key({ ctrlKey: true, key: "v" }), true)).toBe("pass");
    expect(copyKey(key({ ctrlKey: true, type: "keyup" }), true)).toBe("pass");
    expect(copyKey(key({ ctrlKey: true, altKey: true }), true)).toBe("pass");
  });
});
