// Composer — bottom input + send, with two transport paths:
//
//  NATIVE (quick-chat sessions, cwd under ~/.roost/chats): the proper omp
//  integration — sessionsChatCommand tunnels {type:"prompt"} to the worker's
//  `omp --mode rpc` child for this session (lazy-started on first command).
//  No PTY, no TUI, no keystroke timing; the reply streams back as ChatFrames.
//
//  LEGACY (regular terminal sessions running the omp TUI): PTY injection —
//  buildPtyPayload + delayed CR, exactly like TerminalChatButton. The chat
//  overlay mirrors what the terminal does.

import { createMemo, createSignal, Show } from "solid-js";
import { coordClient } from "../../../connect.ts";
import { rootStore } from "../../../store/root.ts";
import { ompChatForSession } from "../../../store/chatOmp.ts";
import { isChatFolder } from "../../../lib/quickChat.ts";
import { addToast } from "../../../lib/toastStore.ts";
import { inputChannel } from "../../../ws/input-channel.ts";
import { buildPtyPayload, CR_BYTES, enterDelayMs } from "../../../lib/ptyPaste.ts";
import { Button } from "../../Settings/md/Button.tsx";
import { TextField } from "../../Settings/md/TextField.tsx";

interface Props {
  sessionId: string;
}

export function Composer(props: Props) {
  const [text, setText] = createSignal("");
  const [sending, setSending] = createSignal(false);

  const isNative = () => isChatFolder(rootStore.sessions[props.sessionId]?.cwd ?? "");
  // Worker-owned turn state (native engine only) — the mirror engine always
  // reports false, so Stop never appears on a PTY-injection session.
  // createMemo, not a bare accessor: the slot may not exist on first render,
  // and a plain read of the fallback literal registers no store dependency.
  const chat = createMemo(() => ompChatForSession(props.sessionId));

  const abort = async () => {
    try {
      await coordClient.sessionsChatCommand({
        sessionId: props.sessionId,
        commandJson: JSON.stringify({ type: "abort" }),
      });
    } catch (e) {
      addToast(`Stop failed: ${e instanceof Error ? e.message : String(e)}`, "err");
    }
  };

  const sendNative = async (body: string) => {
    try {
      const res = await coordClient.sessionsChatCommand({
        sessionId: props.sessionId,
        commandJson: JSON.stringify({ type: "prompt", message: body }),
      });
      const parsed: unknown = JSON.parse(res.responseJson || "{}");
      const ok = !!parsed && typeof parsed === "object" && "success" in parsed && parsed.success === true;
      if (!ok) {
        const err = parsed && typeof parsed === "object" && "error" in parsed ? String(parsed.error) : "prompt rejected";
        addToast(`Chat: ${err}`, "err");
      }
    } catch (e) {
      addToast(`Chat send failed: ${e instanceof Error ? e.message : String(e)}`, "err");
    } finally {
      setSending(false);
    }
  };

  const send = () => {
    const body = text().trim();
    if (!body || sending()) return;
    setSending(true);
    setText("");
    if (isNative()) {
      void sendNative(body);
      return;
    }
    // Legacy TUI path: bracket multi-line, send payload, delayed CR to submit.
    inputChannel.sendInput(props.sessionId, buildPtyPayload(body));
    setTimeout(() => {
      inputChannel.sendInput(props.sessionId, CR_BYTES);
      setSending(false);
    }, enterDelayMs(body));
  };

  const onKey = (e: KeyboardEvent) => {
    // An IME candidate commit fires Enter too; sending there ships a
    // half-composed CJK message.
    if (e.isComposing) return;
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  };

  return (
    <div class="omp-composer" data-testid="omp-chat-composer">
      <TextField
        class="omp-composer__input"
        type="textarea"
        rows={1}
        value={text()}
        onInput={setText}
        placeholder="Send to omp…"
        ariaLabel="Message"
        disabled={sending()}
        testId="omp-chat-input"
        onKeyDown={onKey}
      />
      {/* Stop sits BESIDE Send, not in place of it: omp accepts a mid-turn
          prompt (the worker queues it as a followUp), so hiding Send would
          wrongly imply follow-ups are blocked. */}
      <Show when={chat().streaming}>
        <Button variant="text" icon="stop" data-testid="omp-chat-stop" onClick={() => void abort()}>Stop</Button>
      </Show>
      <Button
        variant="filled"
        icon="send"
        data-testid="omp-chat-send"
        onClick={() => send()}
        disabled={sending() || text().trim().length === 0}
      >
        <Show when={!sending()} fallback="…">Send</Show>
      </Button>
    </div>
  );
}
