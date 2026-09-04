// Canonical Material outlined text field shared by settings and public account forms.
// Callers own field state; this wrapper forwards validation, accessibility, and focus
// properties to the form-associated Material host without exposing its shadow input.

import { type JSX, type Component } from "solid-js";
import { Dynamic } from "solid-js/web";
import type { MdOutlinedTextField } from "@material/web/textfield/outlined-text-field.js";
import "@material/web/textfield/outlined-text-field.js";

export type TextFieldElement = MdOutlinedTextField;

export const TextField: Component<{
  value: string;
  onInput: (value: string) => void;
  label?: string;
  type?: string;
  placeholder?: string;
  class?: string;
  style?: JSX.CSSProperties;
  testId?: string;
  rows?: number;
  min?: number;
  max?: number;
  autocomplete?: string;
  inputMode?: string;
  required?: boolean;
  minLength?: number;
  maxLength?: number;
  ariaDescribedBy?: string;
  autofocus?: boolean;
  onKeyDown?: (event: KeyboardEvent) => void;
  disabled?: boolean;
  ariaLabel?: string;
  ref?: (element: TextFieldElement) => void;
}> = (props) => (
  <Dynamic
    component="md-outlined-text-field"
    ref={(element: HTMLElement) => props.ref?.(element as TextFieldElement)}
    prop:value={props.value}
    label={props.label}
    type={props.type ?? "text"}
    rows={props.rows}
    min={props.min}
    max={props.max}
    prop:autocomplete={props.autocomplete ?? ""}
    prop:inputMode={props.inputMode ?? ""}
    prop:required={props.required ?? false}
    prop:minLength={props.minLength ?? -1}
    prop:maxLength={props.maxLength ?? -1}
    placeholder={props.placeholder}
    class={props.class}
    style={props.style}
    attr:data-testid={props.testId}
    attr:aria-describedby={props.ariaDescribedBy}
    attr:autofocus={props.autofocus ? "" : undefined}
    prop:disabled={props.disabled ?? false}
    attr:aria-label={props.ariaLabel}
    on:keydown={props.onKeyDown}
    on:input={(event: Event) =>
      props.onInput((event.currentTarget as TextFieldElement).value)
    }
  />
);
