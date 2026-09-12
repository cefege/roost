// Native chip primitive for compact labels and actions.
// Its host is passive unless callers provide an onClick callback.
// Shared control CSS owns the visual treatment.

import { type Component, Show } from "solid-js";
import { Icon } from "./Icon.tsx";

export type ChipProps = {
  label: string;
  icon?: string;
  selected?: boolean;
  onClick?: () => void;
  testId?: string;
};

export const Chip: Component<ChipProps> = (props) => {
  const content = (
    <>
      <Show when={props.icon}>
        <Icon name={props.icon!} size="sm" />
      </Show>
      {props.label}
    </>
  );

  return props.onClick ? (
    <button
      type="button"
      class="roost-chip"
      data-selected={props.selected ? "true" : undefined}
      aria-pressed={props.selected === undefined ? undefined : props.selected}
      data-testid={props.testId}
      onClick={props.onClick}
    >
      {content}
    </button>
  ) : (
    <span
      class="roost-chip"
      data-selected={props.selected ? "true" : undefined}
      data-testid={props.testId}
    >
      {content}
    </span>
  );
};
