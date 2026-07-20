// TerminalChatButton — the light chat FAB (chat icon, sibling above the mic and
// below the keyboard-nav FAB). Mirrors the mic's one-shot rhythm: tap → a
// full-width composer bar docks above the soft keyboard (paperclip + textarea +
// send) → type a line → Enter (or send) fires it to the PTY and the bar
// collapses back to the FAB. Unlike the keyboard sheet, nothing is sticky or
// persisted: the draft is ephemeral and Escape/collapse discards it. Typed text
// rides the same bracketed-paste + delayed-CR path as mic dictation
// (lib/ptyPaste). Only one pane's composer opens at a time (module-level guard).
// Styling: styles/voice-input.css (.term-chat, .term-chat-toggle). Caller:
// CellTerminal.tsx (compact/touch/keyboardOnDesktop).

import { createSignal, onCleanup, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { inputChannel } from "../ws/input-channel.ts";
import { onFabPointerDown } from "../lib/fabDragOffset.ts";
import { buildPtyPayload, CR_BYTES, enterDelayMs } from "../lib/ptyPaste.ts";
import { pickAndAttachFiles } from "../lib/attachments.ts";
import type { Session } from "@roost/shared/wire";

interface Props {
  session: Session;
  refocusTerminal?: () => void;
}

// Shared across all deck-mounted chat FABs (one per open session). Only one
// pane's composer is ever open — the owning instance renders the pill; all
// others still render the FAB but never expand, so no two position:fixed pills
// overlap when multiple terminals are visible.
export const [activeChatChannel, setActiveChatChannel] = createSignal<number | null>(null);

export function TerminalChatButton(props: Props) {
  // If another session owns the open composer, don't render at all.
  const owner = activeChatChannel();
  if (owner !== null && owner !== props.session.channel) return null;

  const [open, setOpen] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  let inputEl: HTMLTextAreaElement | undefined;

  const send = (bytes: Uint8Array) => inputChannel.sendInput(props.session.id, bytes);

  const openComposer = () => {
    setOpen(true);
    setActiveChatChannel(props.session.channel);
    queueMicrotask(() => inputEl?.focus());
  };

  const closeComposer = () => {
    setOpen(false);
    setDraft("");
    setActiveChatChannel(null);
    props.refocusTerminal?.();
  };

  // Send the typed line through the same bracketed-paste + delayed-CR path as
  // mic dictation, then collapse (one-shot, like the mic) instead of staying open.
  const sendLine = () => {
    const text = draft().trim();
    if (text.length === 0) return;
    send(buildPtyPayload(text));
    setTimeout(() => send(CR_BYTES), enterDelayMs(text));
    closeComposer();
  };

  onCleanup(() => {
    if (activeChatChannel() === props.session.channel) setActiveChatChannel(null);
  });

  return (
    <div class="term-chat" data-testid="mobile-chat-input" data-open={open() ? "true" : "false"}>
      <Show
        when={open()}
        fallback={
          <button
            type="button"
            class="term-chat-toggle"
            data-testid="terminal-chat-toggle"
            data-open="false"
            aria-label="Type a message"
            onPointerDown={onFabPointerDown}
            onClick={openComposer}
          >
            <span class="term-chat-toggle__icon">chat</span>
          </button>
        }
      >
        <Portal>
        <div class="term-chat__bar" data-testid="chat-bar">
          <button
            type="button"
            class="term-chat__collapse"
            data-testid="chat-collapse"
            onMouseDown={(e) => e.preventDefault()}
            onClick={closeComposer}
            aria-label="Close chat"
          >
            <span class="term-chat__icon">keyboard_arrow_down</span>
          </button>
          <button
            type="button"
            class="term-chat__attach"
            data-testid="chat-attach"
            // MOUSEDOWN preventDefault: keep composer focus so the injected
            // path doesn't land in a blurred pane.
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => pickAndAttachFiles(props.session)}
            aria-label="Attach a file"
          >
            <span class="term-chat__icon">attach_file</span>
          </button>
          <textarea
            class="term-chat__input"
            data-testid="chat-input"
            rows={1}
            placeholder="Type a message…"
            value={draft()}
            onInput={(e) => setDraft(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                sendLine();
              } else if (e.key === "Escape") {
                e.preventDefault();
                closeComposer();
              }
            }}
            ref={(el) => { inputEl = el; }}
          />
          <button
            type="button"
            class="term-chat__send"
            data-testid="chat-send"
            onMouseDown={(e) => e.preventDefault()}
            onClick={sendLine}
            aria-label="Send message"
          >
            <span class="term-chat__icon">send</span>
          </button>
        </div>
        </Portal>
      </Show>
    </div>
  );
}
