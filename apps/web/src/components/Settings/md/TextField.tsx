// Native text field primitive for settings and account forms.
// It owns native input semantics while the wrapper supplies shared layout hooks.
// Callers retain controlled state and receive the focused native element.

import { type Component, type JSX } from "solid-js";

export type TextFieldElement = HTMLInputElement | HTMLTextAreaElement;

export type TextFieldProps = {
  value: string;
  onInput: (value: string) => void;
  label?: string;
  type?: string;
  placeholder?: string;
  class?: string;
  style?: JSX.CSSProperties;
  controlStyle?: JSX.CSSProperties;
  testId?: string;
  rows?: number;
  min?: number;
  max?: number;
  autocomplete?: string;
  inputMode?: JSX.InputHTMLAttributes<HTMLInputElement>["inputMode"];
  required?: boolean;
  minLength?: number;
  maxLength?: number;
  ariaDescribedBy?: string;
  autofocus?: boolean;
  onKeyDown?: (event: KeyboardEvent) => void;
  disabled?: boolean;
  ariaLabel?: string;
  ref?: (element: TextFieldElement) => void;
  id?: string;
  description?: JSX.Element;
  error?: JSX.Element;
  ariaInvalid?: boolean;
};

export const TextField: Component<TextFieldProps> = (props) => {
  const generatedId = `roost-text-field-${globalThis.crypto.randomUUID()}`;
  const controlId = () => props.id ?? generatedId;
  const descriptionId = () => `${controlId()}-description`;
  const errorId = () => `${controlId()}-error`;
  const invalid = () => props.ariaInvalid || Boolean(props.error);
  const describedBy = () => {
    const ids = [
      props.ariaDescribedBy,
      props.description ? descriptionId() : undefined,
      props.error ? errorId() : undefined,
    ].filter((id): id is string => Boolean(id));
    return ids.length > 0 ? ids.join(" ") : undefined;
  };

  return (
    <div
      class={props.class ? `roost-text-field ${props.class}` : "roost-text-field"}
      style={props.style}
      data-invalid={invalid() ? "true" : undefined}
    >
      {props.label && (
        <label class="roost-text-field__label" for={controlId()}>
          {props.label}
        </label>
      )}
      {props.type === "textarea" ? (
        <textarea
          ref={(element) => props.ref?.(element)}
          id={controlId()}
          class="roost-text-field__control"
          style={props.controlStyle}
          value={props.value}
          rows={props.rows}
          placeholder={props.placeholder}
          autocomplete={props.autocomplete}
          inputMode={props.inputMode}
          required={props.required}
          minLength={props.minLength}
          maxLength={props.maxLength}
          autofocus={props.autofocus}
          disabled={props.disabled}
          data-testid={props.testId}
          aria-describedby={describedBy()}
          aria-invalid={invalid() ? "true" : undefined}
          aria-label={props.ariaLabel}
          onKeyDown={props.onKeyDown}
          onInput={(event) => props.onInput(event.currentTarget.value)}
        />
      ) : (
        <input
          ref={(element) => props.ref?.(element)}
          id={controlId()}
          class="roost-text-field__control"
          style={props.controlStyle}
          type={props.type ?? "text"}
          value={props.value}
          min={props.min}
          max={props.max}
          placeholder={props.placeholder}
          autocomplete={props.autocomplete}
          inputMode={props.inputMode}
          required={props.required}
          minLength={props.minLength}
          maxLength={props.maxLength}
          autofocus={props.autofocus}
          disabled={props.disabled}
          data-testid={props.testId}
          aria-describedby={describedBy()}
          aria-invalid={invalid() ? "true" : undefined}
          aria-label={props.ariaLabel}
          onKeyDown={props.onKeyDown}
          onInput={(event) => props.onInput(event.currentTarget.value)}
        />
      )}
      {props.description && (
        <div id={descriptionId()} class="roost-text-field__description">
          {props.description}
        </div>
      )}
      {props.error && (
        <div id={errorId()} class="roost-text-field__error">
          {props.error}
        </div>
      )}
    </div>
  );
};
