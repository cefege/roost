// One subscribed omp sub-agent. The row stays compact while it runs; accumulated
// progress is available behind the same native disclosure used for thinking.

import { Show, type Component } from "solid-js";
import { StatusDot } from "../Settings/md/StatusDot.tsx";
import { Surface } from "../Settings/md/Surface.tsx";
import type { AgentSubagentEntry } from "@roost/shared/wire/agent-entry";

const DOT_STATUS = {
  running: "running",
  done: "ok",
  failed: "error",
  aborted: "info",
} as const satisfies Record<AgentSubagentEntry["state"], "running" | "ok" | "error" | "info">;

const STATE_LABEL = {
  running: "Running",
  done: "Done",
  failed: "Failed",
  aborted: "Aborted",
} as const satisfies Record<AgentSubagentEntry["state"], string>;

const BODY = {
  "white-space": "pre-wrap",
  "overflow-wrap": "anywhere",
  "font-size": "var(--md-body-m-size)",
  "line-height": "var(--md-body-m-line)",
} as const;

export const EntrySubagent: Component<{ entry: AgentSubagentEntry }> = (props) => (
  <div
    data-testid="agent-entry-subagent"
    data-seq={props.entry.seq}
    data-state={props.entry.state}
  >
    <Surface
      level={1}
      radius="md"
      pad={3}
      border
      style={{ "max-width": "min(46rem, 88%)", color: "var(--md-on-surface)" }}
    >
      <div
        style={{
          display: "flex",
          "align-items": "flex-start",
          gap: "var(--md-space-2)",
          "min-width": 0,
        }}
      >
        <span style={{ display: "inline-flex", "padding-top": "var(--md-space-1)" }}>
          <StatusDot status={DOT_STATUS[props.entry.state]} title={props.entry.state} />
        </span>
        <Show
          when={props.entry.text}
          fallback={
            <span
              style={{
                "min-width": 0,
                overflow: "hidden",
                "text-overflow": "ellipsis",
                "white-space": "nowrap",
                "font-size": "var(--md-label-l-size)",
                "line-height": "var(--md-label-l-line)",
                "font-weight": "var(--md-label-l-weight)",
              }}
            >
              {props.entry.name || "Sub-agent"}
            </span>
          }
        >
          <details style={{ flex: "1", "min-width": 0 }}>
            <summary
              style={{
                cursor: "pointer",
                color: "var(--md-on-surface)",
                "font-size": "var(--md-label-l-size)",
                "line-height": "var(--md-label-l-line)",
                "font-weight": "var(--md-label-l-weight)",
                "list-style": "revert",
              }}
            >
              {props.entry.name || "Sub-agent"}
            </summary>
            <div
              style={{
                "margin-top": "var(--md-space-2)",
                "padding-left": "var(--md-space-3)",
                "border-left": "2px solid var(--md-outline-variant)",
                color: "var(--md-on-surface-variant)",
                ...BODY,
              }}
            >
              {props.entry.text}
            </div>
          </details>
        </Show>
        <span
          style={{
            "margin-left": "auto",
            "flex-shrink": 0,
            color: "var(--md-on-surface-dim)",
            "font-size": "var(--md-label-s-size)",
            "line-height": "var(--md-label-s-line)",
          }}
        >
          {STATE_LABEL[props.entry.state]}
        </span>
      </div>
    </Surface>
  </div>
);
