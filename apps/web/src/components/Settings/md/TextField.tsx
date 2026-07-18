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
    on:keydown={props.onKeyDown}
    on:input={(e: Event) =>
      props.onInput((e.currentTarget as HTMLInputElement & { value: string }).value)
    }
  />
);
