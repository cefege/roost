// Non-interactive terminal-capture lease row for the context menu: owns the
// phase-to-StatusDot mapping and the operator-visible lease label.
// TerminalContextMenu renders it in both the floating and compact-sheet branch.
// Labels carry fixed error codes only — never terminal text.
// Depends on lib/terminalIncidentCapture.ts for the lease UI state shape and the
// M3 StatusDot primitive.

import { Show, type Component } from "solid-js";
import { StatusDot } from "./Settings/md/StatusDot.tsx";
import type { TerminalCaptureUiState } from "../lib/terminalIncidentCapture.ts";

const CAPTURE_DOT_STATUS: Record<TerminalCaptureUiState["phase"], string> = {
  idle: "idle",
  arming: "info",
  recording: "running",
  expired: "warn",
  error: "error",
};

/** Lease state as its own non-interactive menu row: recording, expired and
 *  failed must stay legible, which a row inside the disabled Start item is not. */
export const CaptureStateRow: Component<{ state: TerminalCaptureUiState }> = (props) => {
  const label = () => terminalCaptureStateLabel(props.state);
  return (
    <Show when={label().length > 0}>
      <div
        data-testid="ctx-capture-state-row"
        role="presentation"
        class="md-label-s"
        data-phase={props.state.phase}
        style={{
          display: "flex",
          "align-items": "center",
          gap: "var(--md-space-2)",
          padding: "var(--md-space-2) var(--md-space-4)",
          color: "var(--text-lo)",
        }}
      >
        <StatusDot status={CAPTURE_DOT_STATUS[props.state.phase]} title={label()} />
        <span class="md-label-s">{label()}</span>
      </div>
    </Show>
  );
};

/** Menu-visible lease state. Fixed error codes only — never a message that
 *  could quote the terminal text a capture was validating. */
function terminalCaptureStateLabel(state: TerminalCaptureUiState): string {
  switch (state.phase) {
    case "recording":
      return state.heldEvidence ? "recording · evidence held" : "recording";
    case "arming":
      return "arming";
    case "expired":
      return "lease expired · start again";
    case "error":
      return `failed · ${state.lastError ?? "internal"}`;
    case "idle":
      return state.heldEvidence ? "evidence held for retry" : "";
  }
}
