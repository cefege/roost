import { type Component } from "solid-js";
import { Dynamic } from "solid-js/web";
import "@material/web/checkbox/checkbox.js";

// ─── Checkbox → real md-checkbox (selection; distinct from Switch=on/off) ────
export const Checkbox: Component<{
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
  testId?: string;
}> = (props) => (
  <Dynamic
    component="md-checkbox"
    prop:checked={props.checked}
    aria-label={props.label}
    attr:data-testid={props.testId}
    on:change={(e: Event) =>
      props.onChange((e.currentTarget as HTMLInputElement & { checked: boolean }).checked)
    }
  />
);
