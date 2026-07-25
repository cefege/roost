import { type JSX, type Component } from "solid-js";
import { Dynamic } from "solid-js/web";
import "@material/web/textfield/outlined-text-field.js";

// ─── Text field → real md-outlined-text-field (floating label, focus, error) ─
export const TextField: Component<{
  value: string;
  onInput: (v: string) => void;
  label?: string;
  type?: string;
  placeholder?: string;
  class?: string;
  style?: JSX.CSSProperties;
  testId?: string;
  // type="textarea" + rows for multiline; min/max for type="number".
  rows?: number;
  min?: number;
  max?: number;
  // Forwarded so inputs that need them (Enter-to-submit / Esc-to-close, or a
  // ref for .focus()) can migrate off the native <input>. md-text-field's host
  // exposes .focus() (delegates to its inner input).
  onKeyDown?: (e: KeyboardEvent) => void;
  // prop:, not attr: — a bare disabled={false} through <Dynamic> can land as
  // the attribute disabled="false", which HTML reads as DISABLED and would
  // permanently lock a field that only disables itself while sending.
  disabled?: boolean;
  // For fields with no visible label (the chat composer): supplies the
  // accessible name md-outlined-text-field otherwise lacks.
  ariaLabel?: string;
  ref?: (el: HTMLElement) => void;
}> = (props) => (
  <Dynamic
    component="md-outlined-text-field"
    ref={props.ref}
    prop:value={props.value}
    label={props.label}
    type={props.type ?? "text"}
    rows={props.rows}
    min={props.min}
    max={props.max}
    placeholder={props.placeholder}
    class={props.class}
    style={props.style}
    attr:data-testid={props.testId}
    prop:disabled={props.disabled ?? false}
    attr:aria-label={props.ariaLabel}
    on:keydown={props.onKeyDown}
    on:input={(e: Event) =>
      props.onInput((e.currentTarget as HTMLInputElement & { value: string }).value)
    }
  />
);
