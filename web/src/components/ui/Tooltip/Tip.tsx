import type { JSX } from "solid-js";
import type { TooltipValue } from "../../../states/tooltipState";
import { tooltip } from "./Tooltip";

// Wraps content that isn't a Button (pills, icons, plain text) so it can carry a tooltip.
export default function Tip(props: { content: TooltipValue; children: JSX.Element }) {
  return (
    <span ref={(element) => tooltip(element, () => props.content)} style={{ display: "inline-flex" }}>
      {props.children}
    </span>
  );
}
