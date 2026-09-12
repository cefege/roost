// Native checkbox primitive for settings controls.
// It returns ownership of checked state to the caller after every native change.
// The input retains browser keyboard, focus, and accessibility behavior.

import { type Component } from "solid-js";

export type CheckboxProps = {
  checked: boolean;
  onChange: (value: boolean) => void;
  label: string;
  testId?: string;
  disabled?: boolean;
  ariaDescribedBy?: string;
};

export const Checkbox: Component<CheckboxProps> = (props) => (
  <input
    class="roost-checkbox"
    type="checkbox"
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
