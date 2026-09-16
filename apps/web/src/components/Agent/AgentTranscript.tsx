// Scrolling conversation column for the /agent pane: prompts, streamed
// assistant text, tool cards, permission asks and run notices, in arrival
// order. Reads the folded timeline; owns no Mecatl calls of its own.
// Follows the bottom while a run streams, the way a terminal does.

import { createEffect, For, Match, Show, Switch } from "solid-js";
import type { PermissionVerdict } from "@stacklok-oss/mecatl-sdk";
import { EmptyState } from "../Settings/md/primitives.tsx";
import { AgentPermissionCard } from "./AgentPermissionCard.tsx";
import { AgentToolCard } from "./AgentToolCard.tsx";
import type { AgentTimelineEntry } from "./agentTimeline.ts";

export function AgentTranscript(props: {
  entries: readonly AgentTimelineEntry[];
  asksActionable: boolean;
  onVerdict: (askId: string, verdict: PermissionVerdict) => void;
}) {
  let scroller: HTMLDivElement | undefined;

  // Deltas land one token at a time, so following the tail has to react to the
  // entry count AND the trailing entry's text, not just to new rows.
  createEffect(() => {
    const entries = props.entries;
    const tail = entries[entries.length - 1];
    void entries.length;
    void (tail !== undefined && "text" in tail ? tail.text.length : 0);
    if (scroller !== undefined) scroller.scrollTop = scroller.scrollHeight;
  });

  return (
    <div
      ref={scroller}
      data-testid="agent-transcript"
      style={{
        flex: "1",
        "min-height": 0,
        overflow: "auto",
        display: "flex",
        "flex-direction": "column",
        gap: "var(--md-space-3)",
        padding: "var(--md-space-4)",
        "padding-bottom": "calc(var(--md-space-4) + var(--kb-offset))",
      }}
    >
      <Show
        when={props.entries.length > 0}
        fallback={
          <EmptyState
            icon="forum"
            title="No messages yet"
            supporting="Send a prompt to start this session."
          />
        }
      >
        <For each={props.entries}>
          {(entry) => (
            <Switch>
              <Match when={entry.kind === "prompt" ? entry : null}>
                {(prompt) => <AgentBubble role="prompt" text={prompt().text} />}
              </Match>
              <Match when={entry.kind === "assistant" ? entry : null}>
                {(assistant) => (
                  <AgentBubble
                    role="assistant"
                    text={assistant().text}
                    streaming={assistant().streaming}
                  />
                )}
              </Match>
              <Match when={entry.kind === "tool" ? entry : null}>
                {(tool) => <AgentToolCard entry={tool()} />}
              </Match>
              <Match when={entry.kind === "ask" ? entry : null}>
                {(ask) => (
                  <AgentPermissionCard
                    entry={ask()}
                    actionable={props.asksActionable}
                    onVerdict={(verdict) => props.onVerdict(ask().askId, verdict)}
                  />
                )}
              </Match>
              <Match when={entry.kind === "notice" ? entry : null}>
                {(notice) => (
                  <span
                    class="md-label-m"
                    data-testid="agent-notice"
                    style={{
                      color: notice().tone === "error"
                        ? "var(--md-sys-color-error)"
                        : "var(--md-sys-color-on-surface-variant)",
                      "overflow-wrap": "anywhere",
                    }}
                  >
                    {notice().text}
                  </span>
                )}
              </Match>
            </Switch>
          )}
        </For>
      </Show>
    </div>
  );
}

function AgentBubble(props: { role: "prompt" | "assistant"; text: string; streaming?: boolean }) {
  const isPrompt = () => props.role === "prompt";
  return (
    <div
      data-testid={isPrompt() ? "agent-prompt" : "agent-assistant"}
      data-streaming={props.streaming ? "true" : undefined}
      class="md-body-m"
      style={{
        "align-self": isPrompt() ? "flex-end" : "stretch",
        "max-width": isPrompt() ? "80%" : "100%",
        padding: "var(--md-space-3)",
        "border-radius": "var(--md-shape-lg)",
        "white-space": "pre-wrap",
        "overflow-wrap": "anywhere",
        background: isPrompt()
          ? "var(--md-sys-color-secondary-container)"
          : "var(--surface-1)",
        color: isPrompt()
          ? "var(--md-sys-color-on-secondary-container)"
          : "var(--md-sys-color-on-surface)",
      }}
    >
      {props.text}
      <Show when={props.streaming}>
        <span
          aria-label="Streaming"
          style={{ color: "var(--md-sys-color-primary)" }}
        >
          {" \u258c"}
        </span>
      </Show>
    </div>
  );
}
