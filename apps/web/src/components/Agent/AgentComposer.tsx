// Prompt box and run controls for the /agent pane.
//
// Mid-run steering is a server capability (`http_steer`), not something a
// client can emulate: without it the composer is disabled for the duration of
// a run and the next prompt opens a new run after the terminal result. With it
// advertised, the same box steers the live run instead of queueing a prompt.

import { Show } from "solid-js";
import { Button, TextField } from "../Settings/md/primitives.tsx";
import type { AgentRunStatus } from "./agentSessionController.ts";

export function AgentComposer(props: {
  value: string;
  status: AgentRunStatus;
  canSteer: boolean;
  onInput: (value: string) => void;
  onSubmit: () => void;
  onCancel: () => void;
}) {
  const live = () => props.status === "running" || props.status === "cancelling";
  const locked = () => props.status === "loading" || (live() && !props.canSteer);
  const submitLabel = () => (live() && props.canSteer ? "Steer" : "Send");
  const ready = () => !locked() && props.value.trim() !== "";

  function onKeyDown(event: KeyboardEvent): void {
    if (event.key !== "Enter" || event.shiftKey || event.altKey) return;
    event.preventDefault();
    if (ready()) props.onSubmit();
  }

  return (
    <div
      style={{
        display: "flex",
        "align-items": "flex-end",
        gap: "var(--md-space-2)",
        padding: "var(--md-space-3) var(--md-space-4)",
        "border-top": "1px solid var(--md-outline-variant)",
        "flex-shrink": 0,
      }}
    >
      <TextField
        type="textarea"
        value={props.value}
        onInput={props.onInput}
        onKeyDown={onKeyDown}
        rows={2}
        disabled={locked()}
        ariaLabel={submitLabel() === "Steer" ? "Steer the running agent" : "Message the agent"}
        placeholder={placeholderFor(props.status, props.canSteer)}
        testId="agent-composer-input"
        style={{ flex: "1", "min-width": 0 }}
      />
      <Show when={live()}>
        <Button
          variant="destructive"
          data-testid="agent-cancel-run"
          disabled={props.status === "cancelling"}
          onClick={props.onCancel}
        >
          {props.status === "cancelling" ? "Cancelling…" : "Cancel"}
        </Button>
      </Show>
      <Button
        variant="default"
        data-testid="agent-composer-send"
        disabled={!ready()}
        onClick={props.onSubmit}
      >
        {submitLabel()}
      </Button>
    </div>
  );
}

function placeholderFor(status: AgentRunStatus, canSteer: boolean): string {
  if (status === "loading") return "Loading session…";
  if (status === "cancelling") return "Cancelling the run…";
  if (status === "running") {
    return canSteer ? "Steer the running agent…" : "Waiting for the run to finish…";
  }
  return "Ask the agent to do something…";
}
