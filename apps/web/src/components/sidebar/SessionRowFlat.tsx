// Flat-density body for SessionRow — the two-line recency list item (headline +
// time, subtitle, server·path, activity/question line). Split out of
// SessionRow.tsx (400-line cap). Presentational: the shared store-derived state
// (relTime / offline / stage / server) is passed in; the flat-only values
// (folder headline, subtitle, path, last message) are pure functions of the
// session, computed here.

import { createMemo, Show } from "solid-js";
import type { Session } from "@roost/shared/wire";
import { lastMessageOf } from "../../store/selectors.ts";
import { shortCwd } from "../../lib/sidebarFormat.ts";
import { folderHeadline, cloudSubtitle } from "../../lib/sessionTitle.ts";
import { CostChip } from "./CostChip.tsx";
import { ViewersChip } from "./ViewersChip.tsx";
import { STAGE_LABEL } from "./SessionRow.constants.ts";
import { FolderGlyph } from "../FolderGlyph.tsx";

interface SessionRowFlatProps {
  session: Session;
  relTime: string;
  serverOnline: boolean;
  serverLabel: string;
  offline: boolean;
  stage: string | null;
  displayStage: string | null;
}

export function SessionRowFlat(props: SessionRowFlatProps) {
  const cloudSub = createMemo(() => cloudSubtitle(props.session));
  const lastMsg = createMemo(() => lastMessageOf(props.session));
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
      {/* Subtitle — what the program reports (claude task / command / last
          message). Smaller + dimmer than the folder headline; skipped on a
          fresh shell with nothing to say. */}
      <Show when={cloudSub()}>
        <span
          class="df-flat-subtitle"
          data-testid={`session-subtitle-${props.session.id}`}
          title={cloudSub()!}
        >{cloudSub()}</span>
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
      {/* Line 3 — activity: status · viewers · cost on its own line, so
          "running" reads loud instead of fighting server·path for width. A
          needs-input agent swaps the line for the question it's blocked on
          (the actionable info outranks the chips). */}
      <Show
        when={!props.offline && props.stage === "needs-input" && lastMsg()?.text}
        fallback={
      <span class="df-flat-activity">
        <Show when={props.displayStage}>
          {(st) => (
            <span
              class="df-stage-text"
              data-stage={st()}
              data-testid={`session-stage-${props.session.id}`}
            >{STAGE_LABEL[st()] ?? st()}</span>
          )}
        </Show>
        <ViewersChip sessionId={props.session.id} />
        <CostChip session={props.session} />
      </span>
        }
      >
        <span
          class="df-flat-question"
          data-testid={`session-question-${props.session.id}`}
          title={lastMsg()!.text}
        >{lastMsg()!.text.slice(0, 140)}</span>
      </Show>
    </span>
  );
}
