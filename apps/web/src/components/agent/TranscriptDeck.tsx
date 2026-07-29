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

import { Show, type Component } from "solid-js";
import { Composer } from "./Composer.tsx";
import { Transcript } from "./Transcript.tsx";
import { TerminalStatusBadge } from "../TerminalStatusBadge.tsx";
import { sessionTitle } from "../../lib/sessionTitle.ts";
import { shortCwd } from "../../lib/sidebarFormat.ts";
import type { Session } from "@roost/shared/wire";

export const TranscriptDeck: Component<{ session: Session }> = (props) => (
  <div
    data-testid="transcript-deck"
    data-session-id={props.session.id}
    style={{
      position: "absolute",
      inset: "0",
      // Above TerminalDeck, which stays mounted underneath so unrelated shell
      // sessions keep their wterm cores alive across an agent visit.
      "z-index": "10",
      display: "flex",
      "flex-direction": "column",
      "min-height": "0",
      background: "var(--md-surface)",
    }}
  >
    <div
      style={{
        display: "flex",
        "align-items": "center",
        gap: "var(--md-space-3)",
        padding: "var(--md-space-2) var(--md-space-4)",
        "border-bottom": "1px solid var(--md-outline-variant)",
        background: "var(--md-surface-container)",
        "flex-shrink": "0",
      }}
    >
      <div style={{ display: "flex", "flex-direction": "column", "min-width": "0", flex: "1" }}>
        <div
          style={{
            color: "var(--md-on-surface)",
            "font-size": "var(--md-title-s-size)",
            "line-height": "var(--md-title-s-line)",
            "font-weight": "var(--md-title-s-weight)",
            overflow: "hidden",
            "text-overflow": "ellipsis",
            "white-space": "nowrap",
          }}
        >
          {sessionTitle(props.session)}
        </div>
        {/* Only when it adds information: an agent session's auto title IS the
            folder basename, so rendering both printed the same word twice. */}
        <Show when={shortCwd(props.session.spawn_cwd ?? props.session.cwd) !== sessionTitle(props.session)}>
          <div
            style={{
              color: "var(--md-on-surface-variant)",
              "font-size": "var(--md-label-s-size)",
              "line-height": "var(--md-label-s-line)",
              overflow: "hidden",
              "text-overflow": "ellipsis",
              "white-space": "nowrap",
            }}
          >
            {shortCwd(props.session.spawn_cwd ?? props.session.cwd)}
          </div>
        </Show>
      </div>
      {/* Same Working / Needs input / Done / Idle vocabulary the sidebar and
          terminal panes read — lib/agentStatus.ts is the single source. */}
      <TerminalStatusBadge session={props.session} />
    </div>

    <Transcript session={props.session} />
    <Composer session={props.session} />
  </div>
);
