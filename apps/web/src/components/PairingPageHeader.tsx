// Page header for standalone pairing surfaces: "Roost" eyebrow, the page h1,
// and optional supporting copy. PairingGatePanel (unauthorized gate) and
// Onboarding (authorized /pair and zero-machine home) render it; styles live
// in Onboarding.css.

import { Show } from "solid-js";
import type { JSX } from "solid-js";

export function PairingPageHeader(props: { title: string; body?: string }): JSX.Element {
  return (
    <header class="pairing-header">
      <span class="md-label-l pairing-header__eyebrow">Roost</span>
      <h1 class="md-headline-s pairing-header__title">{props.title}</h1>
      <Show when={props.body}>
        <p class="md-body-m pairing-header__body">{props.body}</p>
      </Show>
    </header>
  );
}
