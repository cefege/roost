// TerminalChatButton — the message FAB (chat icon), a sibling above the mic and
// below the keyboard-nav FAB in the corner stack. A faithful analogue of the
// mic: tap → a compact composer (paperclip + textarea + send) with the FAB
// transformed into a close (✕) below it. While open it is PORTALED to <body>
// (so it escapes the deck's swipe transform and anchors to the viewport) and
// docked flush above the soft keyboard via --kb-offset — never covered. Enter
// or the send button fires the line; on send/cancel the composer collapses, the
// keyboard drops (blur on touch — refocusing the terminal would keep it up),
// and the normal FAB row returns. Nothing is sticky or persisted: the draft is
// ephemeral, Escape/✕ discards it. Typed text rides the same bracketed-paste +
// delayed-CR path as mic dictation (lib/ptyPaste). Only one pane's composer is
// open at a time (module-level guard). Styling: styles/voice-input.css
// (.term-chat, .term-chat__dock). Caller: CellTerminal.tsx.

import { createSignal, onCleanup, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { inputChannel } from "../ws/input-channel.ts";
import { onFabPointerDown } from "../lib/fabDragOffset.ts";
import { buildPtyPayload, CR_BYTES, enterDelayMs } from "../lib/ptyPaste.ts";
import { pickAndAttachFiles } from "../lib/attachments.ts";
import { isTouchDevice } from "../lib/windowSizeClass.ts";
import type { Session } from "@roost/shared/wire";

interface Props {
  session: Session;
  refocusTerminal?: () => void;
}

// Shared across all deck-mounted chat FABs (one per open session). Only one
// pane's composer is ever open — the owning instance renders the dock; all
// others still render the FAB but never expand.
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
    // On touch, drop the soft keyboard by blurring — refocusing the terminal's
    // textarea would keep the keyboard up. Desktop keeps terminal focus.
    if (isTouchDevice()) inputEl?.blur();
    else props.refocusTerminal?.();
  };

  // Send the typed line through the same bracketed-paste + delayed-CR path as
  // mic dictation, then collapse (one-shot, like the mic).
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
    <Show
      when={open()}
      fallback={
        <div class="term-chat" data-testid="mobile-chat-input">
          <button
            type="button"
            class="term-chat-toggle"
            data-testid="terminal-chat-toggle"
            aria-label="Type a message"
            onPointerDown={onFabPointerDown}
            onClick={openComposer}
          >
            <span class="term-chat-toggle__icon">chat</span>
          </button>
        </div>
      }
    >
      <Portal>
        <div class="term-chat__dock" data-testid="mobile-chat-input" data-open="true">
          <button
            type="button"
            class="term-chat-toggle"
            data-testid="terminal-chat-toggle"
            data-open="true"
            aria-label="Cancel message"
            onClick={closeComposer}
          >
            <span class="term-chat-toggle__icon">close</span>
          </button>
          <div class="term-chat__box" data-testid="chat-box">
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
        </div>
      </Portal>
    </Show>
  );
}
