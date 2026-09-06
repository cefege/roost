// Material chip primitive for compact actions and selected filters.
// Callers opt into filter-chip semantics by passing selected; assist chips
// remain the default for stateless actions.

import { type Component, Show } from "solid-js";
import { Dynamic } from "solid-js/web";
import "@material/web/chips/assist-chip.js";
import "@material/web/chips/filter-chip.js";
import { Icon } from "./Icon.tsx";

export const Chip: Component<{
  label: string;
  icon?: string;
  selected?: boolean;
  onClick?: () => void;
  testId?: string;
}> = (props) => (
  <Dynamic
    component={props.selected === undefined ? "md-assist-chip" : "md-filter-chip"}
    label={props.label}
    selected={props.selected}
    aria-pressed={props.selected}
    data-testid={props.testId}
    onClick={props.onClick}
  >
    <Show when={props.icon}>
      <span slot="icon" style={{ display: "inline-flex" }}>
        <Icon name={props.icon!} size="sm" />
      </span>
    </Show>
  </Dynamic>
);
