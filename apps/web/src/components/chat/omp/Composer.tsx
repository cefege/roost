// Composer — bottom input + send. Reuses the infra path verbatim:
// buildPtyPayload + CR_BYTES + enterDelayMs (lib/ptyPaste) + inputChannel
// (ws/input-channel), exactly like TerminalChatButton.tsx:17-19,60.
// Enter/Send writes the line to the PTY → omp receives it → omp appends a user
// message → watcher emits it → chat updates. Text-only (attachments v2).

import { createSignal, Show } from "solid-js";
import { inputChannel } from "../../../ws/input-channel.ts";
import { buildPtyPayload, CR_BYTES, enterDelayMs } from "../../../lib/ptyPaste.ts";

interface Props {
  sessionId: string;
}

export function Composer(props: Props) {
  const [text, setText] = createSignal("");
  const [sending, setSending] = createSignal(false);

  const send = () => {
    const body = text().trim();
    if (!body || sending()) return;
    setSending(true);
    // Same path as TerminalChatButton: bracket multi-line, send payload, then
    // a delayed CR to submit (omp needs time to ingest before Enter).
    inputChannel.sendInput(props.sessionId, buildPtyPayload(body));
    setText("");
    setTimeout(() => {
      inputChannel.sendInput(props.sessionId, CR_BYTES);
      setSending(false);
    }, enterDelayMs(body));
  };

  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  };

  return (
    <div class="omp-composer" data-testid="omp-chat-composer">
      <textarea
        class="omp-composer__input"
        placeholder="Send to omp…"
        value={text()}
        onInput={(e) => setText(e.currentTarget.value)}
        onKeyDown={onKey}
        rows={1}
        disabled={sending()}
      />
      <button
        class="omp-composer__send"
        onClick={() => send()}
        disabled={sending() || text().trim().length === 0}
      >
        <Show when={!sending()} fallback="…">Send</Show>
      </button>
    </div>
  );
}
