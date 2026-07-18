import { type Component, Show } from "solid-js";
import { Dynamic } from "solid-js/web";
import "@material/web/chips/assist-chip.js";
import { Icon } from "./Icon.tsx";

// ─── Chip → real md-assist-chip (state-layer + shape + a11y) ──────────────────
export const Chip: Component<{
  label: string;
  icon?: string;
  onClick?: () => void;
  testId?: string;
}> = (props) => (
  <Dynamic component="md-assist-chip" label={props.label} data-testid={props.testId} onClick={props.onClick}>
    <Show when={props.icon}>
      <span slot="icon" style={{ display: "inline-flex" }}>
        <Icon name={props.icon!} size="sm" />
      </span>
    </Show>
  </Dynamic>
);
