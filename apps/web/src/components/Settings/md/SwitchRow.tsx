// Shared preference row for a labelled boolean setting.
// It keeps supporting copy associated with the native switch for assistive tech.
// Settings panes own preference state and provide the visible copy.

import { createUniqueId, type Component, Show } from "solid-js";
import { Switch } from "./Switch.tsx";

export type SwitchRowProps = {
  headline: string;
  support?: string;
  checked: boolean;
  onChange: (value: boolean) => void;
  testId?: string;
  disabled?: boolean;
};

export const SwitchRow: Component<SwitchRowProps> = (props) => {
  const supportId = `roost-switch-row-${createUniqueId()}-support`;

  return (
    <div style={{ display: "flex", "align-items": "center", gap: "var(--md-space-4)" }}>
      <div style={{ flex: 1, "min-width": 0 }}>
        <div class="md-body-m" style={{ color: "var(--md-sys-color-on-surface)" }}>
          {props.headline}
        </div>
        <Show when={props.support}>
          <div
            id={supportId}
            class="md-body-s"
            style={{ color: "var(--md-sys-color-on-surface-variant)" }}
          >
            {props.support}
          </div>
        </Show>
      </div>
      <Switch
        checked={props.checked}
        onChange={props.onChange}
        label={props.headline}
        testId={props.testId}
        disabled={props.disabled}
        ariaDescribedBy={props.support ? supportId : undefined}
      />
    </div>
  );
};
