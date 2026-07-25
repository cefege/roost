// TerminalComposeButton — the PTY text-entry FAB, a sibling above the mic and
// below the keyboard-nav FAB in the corner stack. A faithful analogue of the
// mic: tap → a compact composer (textarea + send) with the FAB
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
//
// TERMINAL MODE ONLY. This types into the session PTY. It was renamed from
// TerminalChatButton so its terminal composition role is explicit.

import { createSignal, onCleanup, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { inputChannel } from "../ws/input-channel.ts";
import { onFabPointerDown } from "../lib/fabDragOffset.ts";
import { buildPtyPayload, CR_BYTES, enterDelayMs } from "../lib/ptyPaste.ts";
import { isTouchDevice } from "../lib/windowSizeClass.ts";
import type { Session } from "@roost/shared/wire";

interface Props {
  session: Session;
  refocusTerminal?: () => void;
}

// Shared across all deck-mounted compose FABs (one per open session). Only one
// pane's composer is ever open — the owning instance renders the dock; all
// others still render the FAB but never expand.
export const [activeComposeChannel, setActiveComposeChannel] = createSignal<number | null>(null);

// True from composer-open until the soft keyboard has finished sliding back out
// on close. AppShell reads this to FREEZE the terminal's --kb-offset reaction
// while the composer owns the keyboard: the composer is a floating bar docked
// above the keyboard, so re-fitting the terminal grid underneath it is pointless
// and is what made the scrollback jump as --kb-offset ramped. Held across the
// dismiss so releasing the freeze doesn't re-fit the grid mid-slide-out.
export const [composerActive, setComposerActive] = createSignal(false);

// Soft-keyboard slide-out window (must cover --kb-offset returning to 0 on
// blur); matches AppShell's --md-sys-motion-duration-medium1 height transition.
const KB_DISMISS_MS = 350;

// Freeze-release generation, module-level so any composer OPEN (even a different
// pane's) invalidates a prior close's pending release — otherwise a close→reopen
// within KB_DISMISS_MS would unfreeze mid-compose. Each open/close bumps it; a
// scheduled release only fires while its captured generation is still current.
let kbReleaseGen = 0;

export function TerminalComposeButton(props: Props) {
  // If another session owns the open composer, don't render at all.
  const owner = activeComposeChannel();
  if (owner !== null && owner !== props.session.channel) return null;

  const [open, setOpen] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  let inputEl: HTMLTextAreaElement | undefined;

  const send = (bytes: Uint8Array) => inputChannel.sendInput(props.session.id, bytes);

  const openComposer = () => {
    setOpen(true);
    setActiveComposeChannel(props.session.channel);
    kbReleaseGen++;
    setComposerActive(true);
    queueMicrotask(() => inputEl?.focus());
  };

  const closeComposer = () => {
    setOpen(false);
    setDraft("");
    setActiveComposeChannel(null);
    // On touch, drop the soft keyboard by blurring — refocusing the terminal's
    // textarea would keep the keyboard up. Desktop keeps terminal focus.
    if (isTouchDevice()) inputEl?.blur();
    else props.refocusTerminal?.();
    // Keep the terminal frozen until the keyboard has fully retracted (--kb-offset
    // back to 0), so lifting the freeze lands on a no-op height, not a re-fit. A
    // later open bumps the generation, no-op'ing this stale release.
    const gen = ++kbReleaseGen;
    setTimeout(() => { if (gen === kbReleaseGen) setComposerActive(false); }, KB_DISMISS_MS);
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
    if (activeComposeChannel() === props.session.channel) {
      setActiveComposeChannel(null);
      setComposerActive(false);
    }
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
            <textarea
              class="term-chat__input"
              data-testid="chat-input"
              rows={1}
              placeholder="Type a message…"
              value={draft()}
              onInput={(e) => {
                const el = e.currentTarget;
                setDraft(el.value);
                el.style.height = "auto";
                el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
              }}
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
