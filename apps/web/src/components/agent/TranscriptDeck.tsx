// The pane for a kind="agent" session: header, transcript, composer. The
// terminal counterpart is TerminalDeck; this one owns no PTY, no cell grid and
// no wterm core — an agent session's backend is an omp RPC child process and
// its content arrives as AgentEntry rows.
//
// v1 deliberately has NO pane tiling: one agent session fills the pane. The
// ~1,900 LOC of PaneStrip/paneLayout/deckOps chrome stays terminal-only; if
// side-by-side transcripts are ever wanted, this component is the mount point.
//
// Caller: components/MainPane.tsx (branches on activeOpenSession().kind).

import { createEffect, type Component, onCleanup } from "solid-js";
import type { Root } from "react-dom/client";
import type { Session } from "@roost/shared/wire";
import { mountOmpSessionSurface } from "./OmpSessionSurface.tsx";

export const TranscriptDeck: Component<{ session: Session }> = (props) => {
  let host!: HTMLDivElement;

  let root: Root | undefined;
  createEffect(() => {
    const sessionId = props.session.id;
    root?.unmount();
    root = mountOmpSessionSurface(host, sessionId);
  });
  onCleanup(() => root?.unmount());

  return (
    <div
      ref={host}
      data-testid="transcript-deck"
      data-session-id={props.session.id}
      style={{
        position: "absolute",
        inset: "0",
        "z-index": "10",
        "min-height": "0",
        background: "var(--md-surface)",
      }}
    />
  );
};
