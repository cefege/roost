// The agent transcript's message box. Enter sends, Shift+Enter newlines —
// the chat convention, not the terminal one.
//
// Sending goes through the EXISTING SessionsUserMessage RPC (coord already
// forwards it to the worker); there is no agent-specific send path. While the
// agent is streaming, Send becomes Stop → SessionsAgentAbort, because the only
// thing a user wants mid-turn is out.
//
// Caller: components/agent/TranscriptDeck.tsx.

import { Show, createSignal, type Component } from "solid-js";
import { AgentButton, AgentTextArea } from "./controls.tsx";
import { liveStatus } from "../../lib/attention.ts";
import { coordClient } from "../../connect.ts";
import { addToast } from "../../lib/toastStore.ts";
import type { Session } from "@roost/shared/wire";

export const Composer: Component<{ session: Session }> = (props) => {
  const [text, setText] = createSignal("");
  const [sending, setSending] = createSignal(false);
  const running = () => liveStatus(props.session) === "running";

  async function send(): Promise<void> {
    const body = text().trim();
    // Gated on !running() so the key path and the button agree: while the
    // agent streams, the composer's action is Stop, not Send. (omp accepts a
    // mid-turn follow-up via streamingBehavior "followUp" — reachable once the
    // turn ends.)
    if (!body || sending() || running()) return;
    setSending(true);
    // Clear first: the user's entry comes back on the transcript stream, and
    // leaving the draft in the box invites a double-send on a slow round-trip.
    setText("");
    try {
      await coordClient.sessionsUserMessage({ sessionId: props.session.id, text: body });
    } catch (err) {
      setText(body);
      addToast(`Send failed: ${err instanceof Error ? err.message : String(err)}`, "err");
    } finally {
      setSending(false);
    }
  }

  async function stop(): Promise<void> {
    try {
      await coordClient.sessionsAgentAbort({ sessionId: props.session.id });
    } catch (err) {
      addToast(`Stop failed: ${err instanceof Error ? err.message : String(err)}`, "err");
    }
  }

  return (
    <div
      data-testid="agent-composer"
      style={{
        display: "flex",
        "align-items": "flex-end",
        gap: "var(--md-space-2)",
        padding: "var(--md-space-3)",
        "border-top": "1px solid var(--md-outline-variant)",
        background: "var(--md-surface-container)",
        "flex-shrink": "0",
      }}
    >
      <AgentTextArea
        value={text()}
        onInput={setText}
        ariaLabel="Message the agent"
        placeholder="Message the agent…"
        testId="agent-composer-input"
        disabled={sending()}
        onKeyDown={(e) => {
          // isComposing: an IME candidate-confirm Enter (CJK, Korean) must
          // land in the box, not fire a half-composed message.
          if (e.key !== "Enter" || e.shiftKey || e.isComposing) return;
          e.preventDefault();
          void send();
        }}
      />
      <Show
        when={running()}
        fallback={
          <AgentButton
            variant="filled"
            disabled={sending() || !text().trim()}
            data-testid="agent-composer-send"
            onClick={() => void send()}
          >
            Send
          </AgentButton>
        }
      >
        <AgentButton variant="tonal" data-testid="agent-composer-stop" onClick={() => void stop()}>
          Stop
        </AgentButton>
      </Show>
    </div>
  );
};
