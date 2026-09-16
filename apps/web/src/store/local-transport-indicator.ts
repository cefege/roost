// Reactive answer to "is this pane talking straight to its own machine's
// worker, skipping the coordinator?" — the one fact the terminal UI surfaces
// about transport choice.
// Callers: PaneTab, PaneTabHoverCard.
// Depends on: terminal-stream-transport.ts, whose change seam ws/local-terminal
// fires whenever the grant set or socket generation moves.

import { createSignal } from "solid-js";
import {
  localTerminalGenerationToken,
  registerTerminalLocalTransportHandler,
} from "./terminal-stream-transport.ts";

const [transportRevision, advanceTransportRevision] = createSignal(0);

/** Called from main.tsx before the first render: a grant that lands during
 *  boot notifies once, and a handler registered after that notification would
 *  leave the marker stale. */
export function installLocalTransportIndicator(): void {
  registerTerminalLocalTransportHandler(() => {
    advanceTransportRevision((revision) => revision + 1);
  });
}

/** True while this session's frames come straight from its own machine's
 *  worker. Deliberately the SAME expression terminalPublicationTarget branches
 *  on, so the marker can never claim a transport the router is not using — a
 *  grant that failed leaves the session on Sync and unmarked. Transport
 *  ownership is plain module state, so the revision signal is what makes a
 *  socket opening or dropping mid-session re-render the marker. */
export function sessionUsesLocalTransport(sessionId: string): boolean {
  transportRevision();
  return localTerminalGenerationToken(sessionId) !== null;
}
