// Material list-row anatomy for static content, actions, and destinations.
// Element semantics follow href first, then onClick, so navigation keeps
// native link affordances while action rows remain buttons.

import { type JSX, type Component, Show } from "solid-js";
import { A } from "@solidjs/router";
import { Icon } from "./Icon.tsx";

// ─── List row ──────────────────────────────────────────────────────
export const ListRow: Component<{
  leading?: string | JSX.Element;
  headline: JSX.Element;
  support?: JSX.Element;
  trailing?: JSX.Element;
  onClick?: () => void;
  href?: string;
  selected?: boolean;
  testId?: string;
  class?: string;
}> = (props) => {
  const inner = (
    <>
      <Show when={props.leading}>
        {(leading) => (
          <div class="md-list-row__leading">
            {typeof leading() === "string"
              ? <Icon name={leading() as string} />
              : (leading() as JSX.Element)}
          </div>
        )}
      </Show>
      <div class="md-list-row__body">
        <div class="md-list-row__headline">{props.headline}</div>
        <Show when={props.support}>
          <div class="md-list-row__support">{props.support}</div>
        </Show>
      </div>
      <Show when={props.trailing}>
        <div class="md-list-row__trailing">{props.trailing}</div>
      </Show>
    </>
  );
  return props.href ? (
    <A
      href={props.href}
      class={props.class ? `md-list-row ${props.class}` : "md-list-row"}
      data-selected={props.selected ? "true" : undefined}
      attr:data-testid={props.testId}
    >
      {inner}
    </A>
  ) : props.onClick ? (
    <button
      type="button"
      class={props.class ? `md-list-row ${props.class}` : "md-list-row"}
      data-selected={props.selected ? "true" : undefined}
      attr:data-testid={props.testId}
      onClick={props.onClick}
    >
      {inner}
    </button>
  ) : (
    <div
      class={props.class ? `md-list-row ${props.class}` : "md-list-row"}
      data-selected={props.selected ? "true" : undefined}
      attr:data-testid={props.testId}
    >
      {inner}
    </div>
  );
};
