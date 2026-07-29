// One message bubble in an agent transcript: the user's prompt, the assistant's
// reply, or a thinking block. Markdown is rendered as PLAIN TEXT with preserved
// whitespace — v1 adds no markdown dependency, and `pre-wrap` keeps code blocks
// and lists legible enough to read a turn.
//
// Thinking sits behind a native <details> disclosure: it is the highest-volume,
// lowest-value content in a transcript, and <summary> gives keyboard + a11y
// toggling for free instead of a hand-rolled button + aria-expanded.
//
// Caller: components/agent/Transcript.tsx.

import { Show, type Component } from "solid-js";
import { Surface } from "../Settings/md/Surface.tsx";
import type { AgentTextEntry } from "@roost/shared/wire/agent-entry";

// Long single-token output (a base64 blob, a path with no spaces) must wrap or
// it forces the whole transcript into a horizontal scroll.
const BODY = {
  "white-space": "pre-wrap",
  "overflow-wrap": "anywhere",
  "font-size": "var(--md-body-m-size)",
  "line-height": "var(--md-body-m-line)",
} as const;

export const EntryText: Component<{ entry: AgentTextEntry }> = (props) => (
  <Show when={props.entry.kind !== "thinking"} fallback={<Thinking entry={props.entry} />}>
    <div
      data-testid={`agent-entry-${props.entry.kind}`}
      data-seq={props.entry.seq}
      style={{
        display: "flex",
        "justify-content": props.entry.kind === "user" ? "flex-end" : "flex-start",
      }}
    >
      <Surface
        level={props.entry.kind === "user" ? 2 : 1}
        radius="md"
        pad={3}
        style={{
          "max-width": "min(46rem, 88%)",
          color: "var(--md-on-surface)",
          ...BODY,
        }}
      >
        {/* An assistant bubble opens empty and fills by delta; an ellipsis is
            the difference between "thinking" and "broken" for that first tick. */}
        {props.entry.text || (props.entry.done ? "" : "…")}
      </Surface>
    </div>
  </Show>
);

const Thinking: Component<{ entry: AgentTextEntry }> = (props) => (
  <details data-testid="agent-entry-thinking" data-seq={props.entry.seq}>
    <summary
      style={{
        cursor: "pointer",
        color: "var(--md-on-surface-variant)",
        "font-size": "var(--md-label-m-size)",
        "line-height": "var(--md-label-m-line)",
        "list-style": "revert",
      }}
    >
      Thinking
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
);
