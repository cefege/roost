// Native Kobalte select primitive for settings and editor forms.
// It translates the public string value into Kobalte's selected option identity.
// Shared control CSS owns the field, menu, and validation presentation.

import * as KobalteSelect from "@kobalte/core/select";
import { type Component, type JSX, createMemo, Show } from "solid-js";
import { Icon } from "./Icon.tsx";

export type SelectOption = {
  value: string;
  label: string;
};

export type SelectProps = {
  value: string;
  onChange: (value: string) => void;
  label?: string;
  options: SelectOption[];
  class?: string;
  testId?: string;
  disabled?: boolean;
  placeholder?: string;
  description?: JSX.Element;
  error?: JSX.Element;
  ariaInvalid?: boolean;
  ariaDescribedBy?: string;
};

export const Select: Component<SelectProps> = (props) => {
  const selectedOption = createMemo(
    () => props.options.find((option) => option.value === props.value) ?? null,
  );
  const invalid = () => props.ariaInvalid || Boolean(props.error);

  return (
    <KobalteSelect.Root<SelectOption>
      class={props.class ? `roost-select ${props.class}` : "roost-select"}
      options={props.options}
      value={selectedOption()}
      optionValue="value"
      optionTextValue="label"
      placeholder={props.placeholder ?? "Select an option"}
      disabled={props.disabled || props.options.length === 0}
      validationState={invalid() ? "invalid" : undefined}
      allowDuplicateSelectionEvents={false}
      onChange={(option) => {
        if (option && option.value !== props.value) props.onChange(option.value);
      }}
      itemComponent={(item) => (
        <KobalteSelect.Item class="roost-select__item" item={item.item}>
          <KobalteSelect.ItemLabel>{item.item.rawValue.label}</KobalteSelect.ItemLabel>
          <KobalteSelect.ItemIndicator class="roost-select__item-indicator">
            <Icon name="check" size="sm" />
          </KobalteSelect.ItemIndicator>
        </KobalteSelect.Item>
      )}
    >
      <Show when={props.label}>
        <KobalteSelect.Label class="roost-select__label">
          {props.label}
        </KobalteSelect.Label>
      </Show>
      <KobalteSelect.Trigger
        class="roost-select__trigger"
        data-testid={props.testId}
        aria-invalid={invalid() ? "true" : undefined}
        aria-describedby={props.ariaDescribedBy}
      >
        <KobalteSelect.Value<SelectOption> class="roost-select__value">
          {(state) => state.selectedOption().label}
        </KobalteSelect.Value>
        <KobalteSelect.Icon class="roost-select__icon" />
      </KobalteSelect.Trigger>
      <Show when={props.description}>
        <KobalteSelect.Description class="roost-select__description">
          {props.description}
        </KobalteSelect.Description>
      </Show>
      <Show when={props.error}>
        <KobalteSelect.ErrorMessage class="roost-select__error">
          {props.error}
        </KobalteSelect.ErrorMessage>
      </Show>
      <KobalteSelect.Portal>
        <KobalteSelect.Content class="roost-select__content">
          <KobalteSelect.Listbox class="roost-select__listbox" />
        </KobalteSelect.Content>
      </KobalteSelect.Portal>
    </KobalteSelect.Root>
  );
};
