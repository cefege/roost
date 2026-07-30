// Flat-density body for SessionRow — the terminal's headline, optional OSC
// subtitle, server/path, worker-offline state, and viewers.

import { createMemo, Show } from "solid-js";
import type { Session } from "@roost/shared/wire";
import { shortCwd } from "../../lib/sidebarFormat.ts";
import { folderHeadline, programSubtitle } from "../../lib/sessionTitle.ts";
import { ViewersChip } from "./ViewersChip.tsx";
import { FolderGlyph } from "../FolderGlyph.tsx";

interface SessionRowFlatProps {
  session: Session;
  relTime: string;
  serverOnline: boolean;
  serverLabel: string;
  offline: boolean;
}

export function SessionRowFlat(props: SessionRowFlatProps) {
  const programSub = createMemo(() => programSubtitle(props.session));
  return (
    <span class="df-flat-body">
      <span class="df-flat-top">
        <span class="df-label df-flat-headline" data-testid={`session-headline-${props.session.id}`}>{folderHeadline(props.session)}</span>
        <span
          class="df-flat-time"
          data-testid={`session-reltime-${props.session.id}`}
          title={props.session.status === "open" ? "Opened" : "Closed"}
        >{props.relTime}</span>
      </span>
      {/* Terminal program detail; omitted for a fresh terminal. */}
      <Show when={programSub()}>
        <span
          class="df-flat-subtitle"
          data-testid={`session-subtitle-${props.session.id}`}
          title={programSub()!}
        >{programSub()}</span>
      </Show>
      {/* Line — location: server · path. Always shown, so "where it
          lives" survives even when the activity line swaps to a question. */}
      <span class="df-flat-supporting">
        <span
          class="df-flat-server"
          data-testid={`session-server-${props.session.id}`}
          data-online={props.serverOnline ? "true" : "false"}
          title={props.serverOnline ? `server: ${props.serverLabel} — online` : `server: ${props.serverLabel} — offline / not running`}
        >
          <svg
            class="df-flat-server-icon"
            width="12" height="12" viewBox="0 0 24 24" fill="none"
            stroke="currentColor" stroke-width="2.2" stroke-linecap="round"
            stroke-linejoin="round" aria-hidden="true"
          >
            <path d="M20 16V7a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v9m16 0H4m16 0 1.28 2.55a1 1 0 0 1-.9 1.45H3.62a1 1 0 0 1-.9-1.45L4 16" />
          </svg>
          <span class="df-flat-server-text">{props.serverLabel}</span>
        </span>
        <span
          class="df-flat-path"
          data-testid={`session-path-${props.session.id}`}
          title={props.session.cwd}
        >
          <FolderGlyph size={11} class="df-flat-folder-icon" />
          <span class="df-flat-path-text">{shortCwd(props.session.cwd)}</span>
        </span>
      </span>
      <span class="df-flat-activity">
        <Show when={props.offline}>
          <span
            class="df-stage-text"
            data-stage="offline"
            data-testid={`session-offline-${props.session.id}`}
          >offline</span>
        </Show>
        <ViewersChip sessionId={props.session.id} />
      </span>
    </span>
  );
}
