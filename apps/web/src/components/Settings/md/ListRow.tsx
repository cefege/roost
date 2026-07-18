import { type JSX, type Component, Show } from "solid-js";
import { Icon } from "./Icon.tsx";

// ─── List row ──────────────────────────────────────────────────────
// Renders <button> for clickable rows and <div> for static rows so
// keyboard navigation and screen readers do the right thing.
export const ListRow: Component<{
  leading?: string | JSX.Element;
  headline: JSX.Element;
  support?: JSX.Element;
  trailing?: JSX.Element;
  onClick?: () => void;
  selected?: boolean;
  testId?: string;
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
  return props.onClick ? (
    <button
      type="button"
      class="md-list-row"
      data-selected={props.selected ? "true" : undefined}
      attr:data-testid={props.testId}
      onClick={props.onClick}
    >
      {inner}
    </button>
  ) : (
    <div
      class="md-list-row"
      data-selected={props.selected ? "true" : undefined}
      attr:data-testid={props.testId}
    >
      {inner}
    </div>
  );
};
