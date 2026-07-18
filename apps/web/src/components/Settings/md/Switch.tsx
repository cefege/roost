import { type Component } from "solid-js";
import { Dynamic } from "solid-js/web";
import "@material/web/switch/switch.js";

// ─── Switch (real M3 md-switch — ripple, keyboard, focus ring for free) ──────
// Replaces the hand-rolled track+thumb <button> that was copy-pasted across
// Settings panes. md-switch dispatches `change`; the new state is on
// e.currentTarget.selected. prop:selected keeps it controlled/reactive.
export const Switch: Component<{
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
  testId?: string;
}> = (props) => (
  <Dynamic
    component="md-switch"
    prop:selected={props.checked}
    aria-label={props.label}
    attr:data-testid={props.testId}
    on:change={(e: Event) =>
      props.onChange((e.currentTarget as HTMLElement & { selected: boolean }).selected)
    }
  />
);
