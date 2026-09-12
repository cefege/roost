// Native boolean switch primitive for settings controls.
// It remains controlled even when a caller ignores or rejects a requested change.
// The input owns keyboard, focus, and accessibility semantics.

import { type Component } from "solid-js";

export type SwitchProps = {
  checked: boolean;
  onChange: (value: boolean) => void;
  label: string;
  testId?: string;
  disabled?: boolean;
  ariaDescribedBy?: string;
};

export const Switch: Component<SwitchProps> = (props) => (
  <input
    class="roost-switch"
    type="checkbox"
    role="switch"
    checked={props.checked}
    disabled={props.disabled}
    data-testid={props.testId}
    aria-label={props.label}
    aria-describedby={props.ariaDescribedBy}
    onChange={(event) => {
      const requested = event.currentTarget.checked;
      try {
        props.onChange(requested);
      } finally {
        event.currentTarget.checked = props.checked;
      }
    }}
  />
);
