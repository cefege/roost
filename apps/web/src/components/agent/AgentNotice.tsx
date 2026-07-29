// Lifecycle chatter (resumed, retries, extension errors). Keep the notice row
// compact; structured source details stay available without overwhelming the
// transcript's primary message flow.

import { Show, createMemo, type Component } from "solid-js";
import type { AgentNoticeEntry } from "@roost/shared/wire/agent-entry";

export const AgentNotice: Component<{ entry: AgentNoticeEntry }> = (props) => {
  const details = createMemo(() => formatDetails(props.entry.details_json));

  return (
    <div
      data-testid="agent-entry-notice"
      data-seq={props.entry.seq}
      data-level={props.entry.level}
      style={{
        color: props.entry.level === "error"
          ? "var(--status-err)"
          : props.entry.level === "warn"
            ? "var(--status-warn)"
            : "var(--md-on-surface-dim)",
        "font-size": "var(--md-label-m-size)",
        "line-height": "var(--md-label-m-line)",
        "white-space": "pre-wrap",
        "overflow-wrap": "anywhere",
        "text-align": "center",
      }}
    >
      {props.entry.text}
      <Show when={props.entry.details_json}>
        <details
          data-testid="agent-entry-notice-details"
          style={{
            "margin-top": "var(--md-space-1)",
            color: "var(--md-on-surface-variant)",
            "text-align": "left",
          }}
        >
          <summary
            style={{
              cursor: "pointer",
              color: "var(--md-on-surface-dim)",
              "font-size": "var(--md-label-s-size)",
              "line-height": "var(--md-label-s-line)",
              "list-style": "revert",
            }}
          >
            Details
          </summary>
          <pre
            style={{
              margin: "var(--md-space-1) 0 0",
              padding: "var(--md-space-2)",
              border: "1px solid var(--md-outline-variant)",
              "border-radius": "var(--md-shape-sm)",
              background: "var(--md-surface-container-low)",
              color: "var(--md-on-surface)",
              "font-family": "var(--term-font-family)",
              "font-size": "var(--md-body-s-size)",
              "line-height": "var(--md-body-s-line)",
              "white-space": "pre-wrap",
              "overflow-wrap": "anywhere",
            }}
          >
            {details()}
          </pre>
        </details>
      </Show>
    </div>
  );
};

function formatDetails(raw: string): string {
  try {
    return JSON.stringify(JSON.parse(raw), null, 2) ?? raw;
  } catch {
    return raw;
  }
}
