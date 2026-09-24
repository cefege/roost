// Inline status line for pairing surfaces. The unauthorized gate mounts no
// NotificationDock, so request, setup-token, and key-recovery outcomes render
// here; PairingGatePanel, PairingOtherOptions, and PairVerificationCodeDialog
// use it. Composes Surface + StatusDot; owns PairingStatusNotice.css.

import type { JSX } from "solid-js";
import { StatusDot, Surface } from "../Settings/md/primitives.tsx";
import "./PairingStatusNotice.css";

export function PairingStatusNotice(props: {
  tone: "ok" | "error";
  message: string;
  testId?: string;
}): JSX.Element {
  return (
    <Surface
      level={2}
      radius="sm"
      pad={3}
      border
      class="pairing-notice"
      role={props.tone === "error" ? "alert" : "status"}
      aria-live={props.tone === "error" ? undefined : "polite"}
      data-testid={props.testId}
    >
      <StatusDot status={props.tone} />
      <span class={`md-body-m pairing-notice__message pairing-notice__message--${props.tone}`}>
        {props.message}
      </span>
    </Surface>
  );
}
