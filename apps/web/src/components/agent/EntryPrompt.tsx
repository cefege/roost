// An omp prompt (tool approval, question, free-text input) rendered INLINE in
// the transcript — deliberately NOT a modal.
//
// omp's approval call site passes no timeout and no signal, and requestRpcDialog
// only arms a timer when the caller supplied one, so an unanswered prompt hangs
// the agent until stdin EOF. A modal can be dismissed with Esc or a backdrop
// click; a dismissed approval would wedge the child process with no way back.
// An inline card cannot be dismissed, only answered — which is the point.
//
// The button row disables optimistically; the AUTHORITATIVE state lands on the
// next AgentEntriesFrame (same seq, `state` flipped) and swaps the card to its
// static answered rendering.
//
// Caller: components/agent/Transcript.tsx.

import { For, Show, createSignal, type Component } from "solid-js";
import { Button } from "../Settings/md/Button.tsx";
import { Surface } from "../Settings/md/Surface.tsx";
import { TextField } from "../Settings/md/TextField.tsx";
import type { AgentPromptEntry } from "@roost/shared/wire/agent-entry";
import { coordClient } from "../../connect.ts";
import { addToast } from "../../lib/toastStore.ts";

export const EntryPrompt: Component<{ sessionId: string; entry: AgentPromptEntry }> = (props) => {
  const [busy, setBusy] = createSignal(false);
  const [freeText, setFreeText] = createSignal("");
  const pending = () => props.entry.state === "pending";
  // omp formats approvals as "Allow tool: bash\n$ ls" — the first line is the
  // question, the rest is the payload being approved and must stay verbatim.
  const head = () => props.entry.title.split("\n", 1)[0] ?? "";
  const detail = () => props.entry.title.slice(head().length).replace(/^\n/, "");

  async function respond(value: string, cancelled: boolean): Promise<void> {
    if (busy() || !pending()) return;
    setBusy(true);
    try {
      await coordClient.sessionsAgentRespond({
        sessionId: props.sessionId,
        promptId: props.entry.prompt_id,
        value,
        cancelled,
      });
    } catch (err) {
      // Re-arm the row: a prompt that never gets answered wedges the agent
      // forever, so a failed send must leave the user able to retry.
      setBusy(false);
      addToast(`Reply failed: ${err instanceof Error ? err.message : String(err)}`, "err");
    }
  }

  return (
    <Surface
      level={2}
      radius="md"
      pad={4}
      border
      style={{
        "border-left": `var(--md-space-1) solid ${pending() ? "var(--md-warning)" : "var(--md-outline-variant)"}`,
      }}
    >
      <div
        data-testid="agent-entry-prompt"
        data-seq={props.entry.seq}
        data-prompt-kind={props.entry.prompt_kind}
        data-state={props.entry.state}
      >
        <div
          style={{
            color: "var(--md-on-surface)",
            "font-size": "var(--md-title-s-size)",
            "line-height": "var(--md-title-s-line)",
            "font-weight": "var(--md-title-s-weight)",
          }}
        >
          {head()}
        </div>

        <Show when={detail()}>
          <pre
            style={{
              margin: "var(--md-space-2) 0 0",
              "white-space": "pre-wrap",
              "overflow-wrap": "anywhere",
              color: "var(--md-on-surface-variant)",
              "font-size": "var(--md-body-s-size)",
              "line-height": "var(--md-body-s-line)",
            }}
          >
            {detail()}
          </pre>
        </Show>

        <Show
          when={pending()}
          fallback={
            <div
              data-testid="agent-prompt-answer"
              style={{
                "margin-top": "var(--md-space-3)",
                color: "var(--md-on-surface-variant)",
                "font-size": "var(--md-body-m-size)",
                "line-height": "var(--md-body-m-line)",
              }}
            >
              {props.entry.state === "cancelled" ? "Cancelled" : `Answered: ${props.entry.answer}`}
            </div>
          }
        >
          <div
            style={{
              display: "flex",
              "flex-wrap": "wrap",
              "align-items": "center",
              gap: "var(--md-space-2)",
              "margin-top": "var(--md-space-3)",
            }}
          >
            <For each={props.entry.options}>
              {(option, i) => (
                <Button
                  variant={i() === 0 ? "filled" : "tonal"}
                  disabled={busy()}
                  data-testid={`agent-prompt-option-${i()}`}
                  onClick={() => void respond(option, false)}
                >
                  {option}
                </Button>
              )}
            </For>
            <Button
              variant="text"
              disabled={busy()}
              data-testid="agent-prompt-cancel"
              onClick={() => void respond("", true)}
            >
              Cancel
            </Button>
          </div>

          <Show when={props.entry.allow_free_text}>
            <div style={{ display: "flex", "align-items": "center", gap: "var(--md-space-2)", "margin-top": "var(--md-space-3)" }}>
              <TextField
                value={freeText()}
                onInput={setFreeText}
                ariaLabel="Your answer"
                placeholder="Type your answer"
                testId="agent-prompt-free-text"
                disabled={busy()}
                style={{ flex: "1" }}
                onKeyDown={(e) => {
                  // isComposing: an IME candidate-confirm Enter must not
                  // submit a half-composed answer.
                  if (e.key !== "Enter" || e.shiftKey || e.isComposing) return;
                  e.preventDefault();
                  if (freeText().trim()) void respond(freeText(), false);
                }}
              />
              <Button
                variant="filled"
                disabled={busy() || !freeText().trim()}
                data-testid="agent-prompt-send"
                onClick={() => void respond(freeText(), false)}
              >
                Send
              </Button>
            </div>
          </Show>
        </Show>
      </div>
    </Surface>
  );
};
