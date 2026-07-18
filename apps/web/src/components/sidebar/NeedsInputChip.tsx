// NeedsInputChip — accent chip for rolled-up needs-input agent count.
// count === 1 → 8×8 pulsing dot. count ≥ 2 → numeric pill.
// Callers: SessionRow, AllView worker-section headers.
// Depends on: nothing external.

import type { Component } from "solid-js";
import { Show } from "solid-js";

interface Props {
  count: number;
  title?: string;
}

export const NeedsInputChip: Component<Props> = (props) => {
  const titleAttr = () =>
    props.title ? `${props.title}: ${props.count}` : `${props.count}`;
  const ariaLabel = () =>
    `${props.count} ${props.count === 1 ? "agent" : "agents"} waiting`;

  return (
    <Show
      when={props.count >= 2}
      fallback={
        <span
          data-testid="needs-input-chip"
          data-variant="dot"
          class="df-needs-pill ml-auto"
          title={titleAttr()}
          aria-label={ariaLabel()}
        >
          needs
        </span>
      }
    >
      <span
        data-testid="needs-input-chip"
        data-variant="pill"
        class="df-needs-pill df-needs-pill-count ml-auto"
        title={titleAttr()}
        aria-label={ariaLabel()}
      >
        {props.count}
      </span>
    </Show>
  );
};
