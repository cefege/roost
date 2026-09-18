// PadHintBar — the transient controller legend: which button does what on the
// surface that currently holds focus. A dock child, so it inherits the bottom
// overlay geometry instead of claiming its own fixed corner. aria-hidden plus
// pointer-events:none keep it out of focus ownership entirely — a screen reader
// gets the same catalogue from HelpOverlay's Controller rows.
// Callers: NotificationDock.tsx. Depends on: lib/padActions, lib/padBindings.

import { For, Show } from "solid-js";
import { BindingChip, Surface } from "./Settings/md/primitives.tsx";
import { padHintContext, padHintsVisible } from "../lib/padActions.ts";
import { PAD_HINTS } from "../lib/padBindings.ts";
import { padModeActive } from "../lib/padMode.ts";

export function PadHintBar() {
  return (
    <Show when={padModeActive() && padHintsVisible()}>
      <Surface
        level={2}
        elevation={3}
        radius="md"
        pad={2}
        class="pad-hint"
        data-testid="pad-hint-bar"
        aria-hidden="true"
      >
        <For each={PAD_HINTS[padHintContext()]}>
          {(hint) => (
            <span class="pad-hint__item">
              <BindingChip>{hint.cap}</BindingChip>
              <span class="md-label-s pad-hint__label">{hint.label}</span>
            </span>
          )}
        </For>
      </Surface>
    </Show>
  );
}
