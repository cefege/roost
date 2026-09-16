// Session picker for one machine's Mecatl daemon: the rows Mecatl's own
// ListSessions returns, plus the "New session" action. No session metadata is
// cached in Roost — the list is re-read from the daemon on every mount.
//
// Rendered by AgentPane.tsx, which owns the client and the selection.

import { For, Show } from "solid-js";
import { Button, EmptyState, List, ListRow, StatusDot } from "../Settings/md/primitives.tsx";
import { relTimeSince } from "../../lib/relTime.ts";

export interface AgentSessionRow {
  sessionId: string;
  title: string;
  state: string;
  turns: number;
  modifiedAtMs: number;
}

const LIVE_STATES: Record<string, true | undefined> = { running: true, awaiting: true };

export function AgentSessionList(props: {
  rows: readonly AgentSessionRow[];
  selectedId: string | null;
  creating: boolean;
  onSelect: (sessionId: string) => void;
  onCreate: () => void;
}) {
  return (
    <div
      data-testid="agent-session-list"
      style={{
        display: "flex",
        "flex-direction": "column",
        gap: "var(--md-space-2)",
        "min-height": 0,
      }}
    >
      <div style={{ display: "flex", "align-items": "center", gap: "var(--md-space-2)" }}>
        <h2 class="md-title-s" style={{ margin: 0, flex: "1" }}>Sessions</h2>
        <Button
          variant="secondary"
          size="sm"
          icon="add"
          data-testid="agent-new-session"
          disabled={props.creating}
          onClick={props.onCreate}
        >
          {props.creating ? "Creating…" : "New session"}
        </Button>
      </div>

      <Show
        when={props.rows.length > 0}
        fallback={
          <EmptyState
            icon="chat_bubble_outline"
            title="No agent sessions yet"
            supporting="Create one to start a conversation on this machine."
          />
        }
      >
        <div style={{ overflow: "auto", "min-height": 0 }}>
          <List contained>
            <For each={props.rows}>
              {(row) => (
                <ListRow
                  leading="smart_toy"
                  headline={row.title}
                  support={`${row.state} · ${row.turns} turn${row.turns === 1 ? "" : "s"} · ${relTimeSince(row.modifiedAtMs)}`}
                  trailing={
                    <StatusDot
                      status={LIVE_STATES[row.state] === true ? "running" : "idle"}
                      hollow={LIVE_STATES[row.state] === undefined}
                      title={row.state}
                    />
                  }
                  selected={row.sessionId === props.selectedId}
                  onClick={() => props.onSelect(row.sessionId)}
                  testId={`agent-session-${row.sessionId}`}
                />
              )}
            </For>
          </List>
        </div>
      </Show>
    </div>
  );
}
