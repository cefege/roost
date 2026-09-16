// One tool call in the agent transcript: the tool's name, its arguments, and
// its result once Mecatl returns one. Rendered by AgentTranscript.tsx from a
// folded `tool.call` / `tool.result` pair.
// Arguments and results are opaque JSON from Mecatl — shown verbatim, never
// parsed or interpreted by Roost.

import { Show } from "solid-js";
import { Card, StatusDot } from "../Settings/md/primitives.tsx";
import type { AgentToolEntry } from "./agentTimeline.ts";

export function AgentToolCard(props: { entry: AgentToolEntry }) {
  const result = () => props.entry.result;
  const status = () => {
    const outcome = result();
    if (outcome === null) return "running";
    return outcome.isError ? "error" : "ok";
  };
  const statusLabel = () => {
    const outcome = result();
    if (outcome === null) return "Running";
    return outcome.isError ? "Failed" : "Done";
  };

  return (
    <Card
      variant="outlined"
      data-testid={`agent-tool-${props.entry.callId}`}
      style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-2)" }}
    >
      <div
        style={{
          display: "flex",
          "align-items": "center",
          gap: "var(--md-space-2)",
          "min-width": 0,
        }}
      >
        <StatusDot status={status()} title={statusLabel()} />
        <span class="md-title-s" style={{ "min-width": 0, "overflow-wrap": "anywhere" }}>
          {props.entry.name}
        </span>
        <span
          class="md-label-m"
          style={{ "margin-left": "auto", color: "var(--md-sys-color-on-surface-variant)" }}
        >
          {statusLabel()}
        </span>
      </div>

      <Show when={props.entry.args !== ""}>
        <pre class="md-body-s" style={preStyle()}>{props.entry.args}</pre>
      </Show>

      <Show when={result()}>
        {(outcome) => (
          <pre
            class="md-body-s"
            data-testid={`agent-tool-result-${props.entry.callId}`}
            style={{
              ...preStyle(),
              color: outcome().isError
                ? "var(--md-sys-color-error)"
                : "var(--md-sys-color-on-surface-variant)",
            }}
          >
            {outcome().content}
          </pre>
        )}
      </Show>
    </Card>
  );
}

// Opaque tool payloads can be long and are frequently one line of JSON, so the
// block scrolls instead of stretching the transcript column.
function preStyle() {
  return {
    margin: 0,
    "max-height": "calc(var(--md-space-9) * 4)",
    overflow: "auto",
    "white-space": "pre-wrap",
    "overflow-wrap": "anywhere",
    color: "var(--md-sys-color-on-surface-variant)",
    "font-family": "var(--font-mono)",
  } as const;
}
