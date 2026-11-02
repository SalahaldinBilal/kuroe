import { A } from "@solidjs/router";
import { type JSX, Show, splitProps } from "solid-js";
import { tooltip } from "../Tooltip/Tooltip";
import styles from "./Button.module.scss";

export type ButtonVariant = "ghost" | "subtle" | "primary" | "danger" | "danger-solid";

export type ButtonProps = {
  variant?: ButtonVariant;
  size?: "sm" | "md";
  icon?: boolean;
  active?: boolean;
  block?: boolean;
  tooltip?: string;
  type?: "button" | "submit";
  form?: string;
  disabled?: boolean;
  href?: string;
  download?: boolean;
  target?: string;
  class?: string;
  style?: JSX.CSSProperties;
  children?: JSX.Element;
  onClick?: (event: MouseEvent & { currentTarget: HTMLElement }) => void;
  ref?: (element: HTMLElement) => void;
};

const VARIANT_CLASS: Record<ButtonVariant, string> = {
  ghost: styles.ghost,
  subtle: styles.subtle,
  primary: styles.primary,
  danger: styles.danger,
  "danger-solid": styles.dangerSolid,
};

function Button(props: ButtonProps) {
  const [local] = splitProps(props, ["variant", "size", "icon", "active", "tooltip", "block"]);
  const className = () => `${styles.Button} ${VARIANT_CLASS[local.variant ?? "subtle"]} ${props.class ?? ""}`;
  const classList = () => ({
    [styles.Small]: local.size === "sm",
    [styles.Icon]: local.icon ?? false,
    [styles.Active]: local.active ?? false,
    [styles.Block]: local.block ?? false,
  });
  const ref = (element: HTMLElement) => {
    tooltip(element, () => local.tooltip);
    props.ref?.(element);
  };

  const internal = () => {
    const href = props.href ?? "";
    return href.startsWith("/") && !/^\/(api|preview)\//.test(href) && !props.download && !props.target;
  };

  return (
    <Show
      when={props.href}
      fallback={
        <button
          ref={ref}
          type={props.type ?? "button"}
          form={props.form}
          class={className()}
          classList={classList()}
          style={props.style}
          disabled={props.disabled}
          aria-label={local.icon ? local.tooltip : undefined}
          onClick={(event) => props.onClick?.(event)}
        >
          {props.children}
        </button>
      }
    >
      <Show
        when={internal()}
        fallback={
          <a
            ref={ref}
            href={props.href}
            download={props.download ? "" : undefined}
            target={props.target}
            rel={props.target ? "noreferrer" : undefined}
            class={className()}
            classList={classList()}
            style={props.style}
            aria-label={local.icon ? local.tooltip : undefined}
            onClick={(event) => props.onClick?.(event)}
          >
            {props.children}
          </a>
        }
      >
        <A
          ref={ref}
          href={props.href ?? ""}
          class={className()}
          classList={classList()}
          style={props.style}
          aria-label={local.icon ? local.tooltip : undefined}
        >
          {props.children}
        </A>
      </Show>
    </Show>
  );
}

export default Button;
