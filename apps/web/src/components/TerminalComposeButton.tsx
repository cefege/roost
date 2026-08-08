// TerminalComposeButton — the PTY text-entry FAB and, once open, the composer
// bar. Closed: a `chat` FAB, a sibling above the mic and below the keyboard-nav
// FAB in the corner stack. Open: one full-width pill —
// [text field] [mic] [send] — PORTALED to <body> (so it escapes the deck's
// swipe transform and anchors to the viewport) and docked flush above the soft
// keyboard via --kb-offset, never covered.
// The bar is ONE ROW at every draft length: the field grows in HEIGHT while the
// controls stay bottom-aligned in the same three grid tracks, so the field's
// width — and therefore its wrap points — never move.
// Send is the ONLY thing that fires the line — Enter inserts a newline, so
// multi-line is normal. Send is absent only while the inline mic holds the
// recording slot (that mic's own ✕ discards the utterance in flight). So there
// is no ✕ anywhere in the bar itself: the closers are tap-outside, Escape and
// send, and only send consumes the draft.
// The mic is MobileVoiceInput variant="inline"; its finalized transcript lands
// in the draft, so send is what commits it. On send/cancel the composer
// collapses, the keyboard drops (blur on touch — refocusing the terminal would
// keep it up), and the normal FAB row returns.
// An unsent draft is RETAINED per session by lib/composerDrafts.ts (this device
// only, never server-side): tap-outside / Escape merely collapse the bar, and
// so does a pane switch, which tears this component down entirely.
// CellTerminal submits the draft through the current terminal mode and applies
// delayed CR ordering. Only one pane's composer is open at a time (module-level
// guard). Styling: styles/voice-input.css (.term-chat, .term-chat__dock,
// .term-chat__box, .term-chat__ctl). Caller: CellTerminal.tsx.
//
// TERMINAL MODE ONLY. This types into the session PTY. It was renamed from
// TerminalChatButton so its terminal composition role is explicit.

import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { MobileVoiceInput, activeVoiceChannel } from "./MobileVoiceInput.tsx";
import { getComposerDraft, saveComposerDraft } from "../lib/composerDrafts.ts";
import { onFabPointerDown } from "../lib/fabDragOffset.ts";
import type { TerminalContext } from "../lib/keytermContext.ts";
import { isTouchDevice } from "../lib/windowSizeClass.ts";
import type { Session } from "@roost/shared/wire";

interface Props {
  session: Session;
  refocusTerminal?: () => void;
  /** Submits the draft through the pane's current terminal mode. */
  onSubmit: (text: string) => void;
  /** Live terminal text for Deepgram keyterm biasing; forwarded to the mic. */
  readContext?: () => TerminalContext;
  initialOpen?: boolean;
  onOpen?: () => void;
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
  let initialOpenConsumed = false;

  const [open, setOpen] = createSignal(false);
  // Captured once at body scope: the write-through effect below must not read
  // props from a deferred callback.
  const sessionId = props.session.id;
  const [draft, setDraft] = createSignal(getComposerDraft(sessionId));
  let inputEl: HTMLTextAreaElement | undefined;
  let dockEl: HTMLDivElement | undefined;

  // The textarea's height is imperative, so any programmatic draft change must
  // re-run it — onInput is no longer the only writer. `height:auto` makes a
  // textarea report its `rows` height, so scrollHeight is the content height;
  // the CSS max-height clamps the assignment (no second copy of the cap here).
  const autoGrow = () => {
    if (!inputEl) return;
    inputEl.style.height = "auto";
    inputEl.style.height = `${inputEl.scrollHeight}px`;
  };

  // A programmatic write must also keep the NEWEST text visible: past
  // max-height the field scrolls, and unlike typing there is no caret motion for
  // the browser to follow, so streamed dictation would scroll out of view. Not
  // used from onInput — the caret already pulls the view there, and forcing it
  // would fight a user who scrolled up to edit an earlier line.
  const growAndFollow = () => {
    autoGrow();
    if (inputEl) inputEl.scrollTop = inputEl.scrollHeight;
  };

  // One rule for gluing text onto a draft: exactly one separating space, never a
  // leading one. Shared by the attachment sink and both dictation paths.
  const glued = (base: string, text: string) =>
    (base.length === 0 || base.endsWith(" ") ? base : `${base} `) + text;

  // Every control in the bar must keep the soft keyboard up: a mousedown that
  // reaches the button blurs the field.
  const keepKeyboard = (e: MouseEvent) => e.preventDefault();

  // Dictation streams into THIS field, not a floating caption — one box for one
  // utterance. Each engine update replaces the live tail, so `dictationBase` is
  // whatever was already typed when the mic opened, held for that one dictation.
  let dictationBase: string | null = null;
  const showDictation = (text: string | null) => {
    dictationBase ??= draft();
    setDraft(text === null ? dictationBase : glued(dictationBase, text));
    if (text === null) dictationBase = null;
    // Deferred like the other programmatic writers: growAndFollow measures
    // scrollHeight, and Solid has not written the new value into the element
    // yet, so measuring here reads the PREVIOUS text and leaves the height (and
    // the scroll) one line behind the speech.
    queueMicrotask(growAndFollow);
  };
  // Stop: keep the settled text and hand the field back the caret.
  const commitDictation = (text: string) => {
    const base = dictationBase ?? draft();
    dictationBase = null;
    setDraft(glued(base, text));
    queueMicrotask(() => {
      inputEl?.focus();
      growAndFollow();
    });
  };

  // The mic owns the recording slot for the whole dictation, so this is the
  // field's cue that speech — not typing — is filling it. Replaces the caption's
  // "Listening…" hint, which the inline mic no longer renders.
  const dictating = () => activeVoiceChannel() === props.session.channel;

  // Tap-outside dismissal: touch has no Escape, so the bar needs this to be
  // closable. Capture phase, because the terminal pane stops propagation on
  // pointer events. It can't dismiss itself on open: openComposer runs from the
  // FAB's click, after that gesture's pointerdown was already dispatched.
  const onDocPointerDown = (e: PointerEvent) => {
    if (dockEl?.contains(e.target as Node)) return;
    // A dictation in flight OWNS the bar: collapsing it unmounts the inline mic
    // and the engine's onCleanup kills the recording with no error and no text.
    // The ✕ beside the mic is the deliberate way to abandon an utterance.
    if (dictating()) return;
    closeComposer();
  };

  const openComposer = () => {
    setOpen(true);
    props.onOpen?.();
    setActiveComposeChannel(props.session.channel);
    kbReleaseGen++;
    setComposerActive(true);
    queueMicrotask(() => inputEl?.focus());
    document.addEventListener("pointerdown", onDocPointerDown, true);
  };

  const closeComposer = () => {
    document.removeEventListener("pointerdown", onDocPointerDown, true);
    setOpen(false);
    // A dictation abandoned by dismissing the bar must release its base —
    // otherwise the next utterance glues itself onto a stale prefix. The draft
    // itself stays (retention); only the in-flight dictation is dropped.
    dictationBase = null;
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

  // Submit the untouched draft through the owning terminal, then collapse. This
  // is the ONLY draft consumer: the effect below flushes "" to storage.
  const sendLine = () => {
    props.onSubmit(draft());
    setDraft("");
    closeComposer();
  };

  // Write-through retention: the component is torn down whenever the pane loses
  // focus (CellTerminal's <Show> on props.focused), so the draft cannot live
  // only in this signal. Per keystroke is deliberate — a mobile tab can be
  // discarded at any moment.
  createEffect(() => saveComposerDraft(sessionId, draft()));
  createEffect(() => {
    const owner = activeComposeChannel();
    if (props.initialOpen !== true || initialOpenConsumed) return;
    if (owner !== null && owner !== props.session.channel) return;
    initialOpenConsumed = true;
    openComposer();
    // CellTerminal's cold-mount focus also settles in animation-frame time.
    // Reassert composer ownership after that pass so automatic open receives
    // typing immediately, matching a user-triggered open after mount.
    requestAnimationFrame(() => inputEl?.focus());
  });


  onCleanup(() => {
    document.removeEventListener("pointerdown", onDocPointerDown, true);
    if (activeComposeChannel() === props.session.channel) {
      setActiveComposeChannel(null);
      setComposerActive(false);
    }
  });

  return (
    <Show when={activeComposeChannel() === null || activeComposeChannel() === props.session.channel}>
      <Show
        when={open()}
        fallback={
          <div class="term-chat" data-testid="mobile-chat-input">
            <button
              type="button"
              class="term-chat-toggle"
              data-testid="terminal-chat-toggle"
              aria-label="Compose terminal input"

              onPointerDown={onFabPointerDown}
              onClick={openComposer}
            >
              <span class="term-chat-toggle__icon">chat</span>
            </button>
          </div>
        }
      >
        <Portal>
          <div
            class="term-chat__dock"
            data-testid="mobile-chat-input"
            data-open="true"
            ref={(el) => { dockEl = el; }}
          >
            <div class="term-chat__box" data-testid="chat-box">
              <textarea
                class="term-chat__input"
                data-testid="chat-input"
                rows={1}
                placeholder={dictating() ? "Listening…" : "Type terminal input…"}
                value={draft()}
                onInput={(e) => {
                  setDraft(e.currentTarget.value);
                  autoGrow();
                }}
                onKeyDown={(e) => {
                  // Enter is a NEWLINE here, never a submit — the send button is the
                  // only thing that commits the draft. A soft keyboard's Return is
                  // the same key a user needs to write a second line, and this bar
                  // sends multi-line input verbatim.
                  if (e.key === "Escape") {
                    e.preventDefault();
                    closeComposer();
                  }
                }}
                ref={(el) => {
                  inputEl = el;
                  // Portal insertion completes after the current microtask;
                  // focus, put the caret after the retained draft, and re-derive
                  // the bar height on the next task, once the textarea is
                  // connected and measurable.
                  setTimeout(() => {
                    el.focus();
                    el.setSelectionRange(el.value.length, el.value.length);
                    growAndFollow();
                  }, 0);
                }}
              />
              <MobileVoiceInput
                variant="inline"
                channelId={props.session.channel}
                onTranscript={commitDictation}
                onLiveTranscript={showDictation}
                onTerminalSubmit={props.onSubmit}
                readContext={props.readContext}
                refocusTerminal={() => inputEl?.focus()}
              />
              {/* Hidden while dictating: the inline mic's own ✕ discard takes the
                  trailing area, sending a half-finalized utterance is never the
                  intent, and the bar keeps exactly three controls in every state
                  (idle [field][mic][send], dictating [field][✕][stop]) so the
                  field never re-wraps mid-utterance. */}
              <Show when={!dictating()}>
                <button
                  type="button"
                  class="term-chat__ctl term-chat__send"
                  data-testid="chat-send"
                  onMouseDown={keepKeyboard}
                  onClick={sendLine}
                  aria-label="Send to terminal"
                >
                  <span class="term-chat__icon">send</span>
                </button>
              </Show>
            </div>
          </div>
        </Portal>
      </Show>
    </Show>
  );
}
