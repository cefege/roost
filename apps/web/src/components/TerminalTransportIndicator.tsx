/*
 * Passive selected-terminal carrier label for desktop and compact terminal headers.
 * PaneStrip and MobileDeckBar supply the selected session; the transport store is
 * the only source of baseline-qualified carrier state.
 */

import { createMemo } from "solid-js";
import { sessionTerminalTransportPresentation } from "../store/local-transport-indicator.ts";
import { Chip } from "./Settings/md/Chip.tsx";

export interface TerminalTransportIndicatorProps {
  sessionId: string;
}

export function TerminalTransportIndicator(props: TerminalTransportIndicatorProps) {
  const presentation = createMemo(() => sessionTerminalTransportPresentation(props.sessionId));

  return (
    <div
      class="terminal-transport-indicator"
      data-testid="terminal-transport-indicator"
      data-session-id={props.sessionId}
      data-terminal-transport={presentation().kind ?? "unconfirmed"}
      role="group"
      aria-label="Terminal transport"
    >
      <Chip label={presentation().label} title={presentation().description} />
    </div>
  );
}
