// TerminalComposeButton — the PTY text-entry FAB and, once open, the composer
// bar. Closed: a `chat` FAB, a sibling above the mic and below the keyboard-nav
// FAB in the corner stack. Open: one full-width pill —
// [+] [text field] [mic] [send] — PORTALED to <body> (so it escapes the deck's
// swipe transform and anchors to the viewport) and docked flush above the soft
// keyboard via --kb-offset, never covered. There is no floating ✕ FAB: the
// leading button is `+` (opens the take-photo / photos / attach-file sheet)
// while the draft is empty and ✕ (discard + close) once there is text; tapping
// outside the bar also closes it. The send button is the ONLY thing that fires
// the line — Enter inserts a newline, so a multi-line draft is normal.
// Attachments picked from the `+` sheet upload through lib/attachments.ts and
// their absolute path is appended to the DRAFT — to the PTY only if the bar
// closed mid-upload. The mic is MobileVoiceInput variant="inline"; its
// finalized transcript lands in the draft, so send is what commits it. On
// send/cancel the composer collapses, the keyboard drops (blur on touch —
// refocusing the terminal would keep it up), and the normal FAB row returns.
// Nothing is sticky or persisted: the draft is ephemeral, Escape/✕ discards it.
// CellTerminal submits the draft through the current terminal mode and applies
// delayed CR ordering. Only one pane's composer is open at a time (module-level
// guard). Styling: styles/voice-input.css (.term-chat, .term-chat__dock,
// .term-chat__box, .term-chat__menu). Caller: CellTerminal.tsx.
//
// TERMINAL MODE ONLY. This types into the session PTY. It was renamed from
// TerminalChatButton so its terminal composition role is explicit.

import { createSignal, onCleanup, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { MobileVoiceInput } from "./MobileVoiceInput.tsx";
import { injectPath, pickFilesTo, type PickOptions } from "../lib/attachments.ts";
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
  const [menuOpen, setMenuOpen] = createSignal(false);
  let inputEl: HTMLTextAreaElement | undefined;
  let dockEl: HTMLDivElement | undefined;
  let menuEl: HTMLDivElement | undefined;
  let leadEl: HTMLButtonElement | undefined;

  // The textarea's height is imperative, so any programmatic draft change must
  // re-run it — onInput is no longer the only writer.
  const autoGrow = () => {
    if (!inputEl) return;
    inputEl.style.height = "auto";
    inputEl.style.height = `${Math.min(inputEl.scrollHeight, 160)}px`;
  };

  // Append with exactly one separating space, never a leading one.
  const appendToDraft = (text: string) => {
    setDraft((d) => (d.length === 0 || d.endsWith(" ") ? d : `${d} `) + text);
    queueMicrotask(() => {
      inputEl?.focus();
      autoGrow();
    });
  };

  // Tap-outside dismissal: with an empty draft the leading control is `+`, and
  // touch has no Escape, so the bar needs this to be closable. Capture phase,
  // because the terminal pane stops propagation on pointer events. It can't
  // dismiss itself on open: openComposer runs from the FAB's click, after that
  // gesture's pointerdown was already dispatched.
  //
  // A tap inside the dock but outside the sheet retires the sheet only — the
  // alternative is a sheet left hanging over the bar after tapping the field.
  // The leading button is excluded: its own click owns the toggle.
  const onDocPointerDown = (e: PointerEvent) => {
    const target = e.target as Node;
    if (dockEl?.contains(target)) {
      if (menuOpen() && !menuEl?.contains(target) && !leadEl?.contains(target)) setMenuOpen(false);
      return;
    }
    closeComposer();
  };

  // Uploads resolve asynchronously; if the bar closed meanwhile, don't drop the
  // upload — fall back to the FAB's behavior and type the path into the PTY.
  const attachSink = (absPath: string) => {
    if (open()) appendToDraft(`${absPath} `);
    else injectPath(props.session, absPath);
  };
  const pickInto = (opts?: PickOptions) => {
    // iOS only honors input.click() inside the gesture — pick BEFORE dismissing.
    pickFilesTo(props.session, attachSink, opts);
    setMenuOpen(false);
  };

  const openComposer = () => {
    setOpen(true);
    setActiveComposeChannel(props.session.channel);
    kbReleaseGen++;
    setComposerActive(true);
    queueMicrotask(() => inputEl?.focus());
    document.addEventListener("pointerdown", onDocPointerDown, true);
  };

  const closeComposer = () => {
    document.removeEventListener("pointerdown", onDocPointerDown, true);
    setOpen(false);
    setDraft("");
    setMenuOpen(false);
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

  // Submit the untouched draft through the owning terminal, then collapse.
  const sendLine = () => {
    props.onSubmit(draft());
    closeComposer();
  };


  onCleanup(() => {
    document.removeEventListener("pointerdown", onDocPointerDown, true);
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
          <Show when={menuOpen() && draft().length === 0}>
            {/* Not role="menu": these rows never take focus (mousedown is
                prevented so the field keeps the keyboard) and there is no
                arrow-key model, so menu semantics would promise a keyboard
                contract this touch sheet does not implement. The trigger's
                aria-haspopup + aria-expanded is the honest description. */}
            <div class="term-chat__menu" data-testid="chat-add-menu" ref={(el) => { menuEl = el; }}>
              <button
                type="button"
                class="term-chat__menu-item"
                data-testid="chat-add-photo"
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => pickInto({ accept: "image/*", capture: "environment", multiple: false })}
              >
                <span class="term-chat__menu-icon">photo_camera</span>Take photo
              </button>
              <button
                type="button"
                class="term-chat__menu-item"
                data-testid="chat-add-gallery"
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => pickInto({ accept: "image/*" })}
              >
                <span class="term-chat__menu-icon">photo_library</span>Photos
              </button>
              <button
                type="button"
                class="term-chat__menu-item"
                data-testid="chat-add-file"
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => pickInto()}
              >
                <span class="term-chat__menu-icon">attach_file</span>Attach file
              </button>
            </div>
          </Show>
          <div class="term-chat__box" data-testid="chat-box">
            <button
              type="button"
              class="term-chat__lead"
              data-testid="chat-lead"
              data-mode={draft().length > 0 ? "clear" : "add"}
              aria-label={draft().length > 0 ? "Discard draft" : "Add attachment"}
              aria-haspopup="menu"
              aria-expanded={menuOpen() && draft().length === 0 ? "true" : "false"}
              ref={(el) => { leadEl = el; }}
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => {
                if (draft().length > 0) closeComposer();
                else setMenuOpen((v) => !v);
              }}
            >
              <span class="term-chat__icon">{draft().length > 0 ? "close" : "add"}</span>
            </button>
            <textarea
              class="term-chat__input"
              data-testid="chat-input"
              rows={1}
              placeholder="Type terminal input…"
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
                  // The sheet is the innermost dismissible layer.
                  if (menuOpen()) setMenuOpen(false);
                  else closeComposer();
                }
              }}
              ref={(el) => { inputEl = el; }}
            />
            <MobileVoiceInput
              variant="inline"
              channelId={props.session.channel}
              onTranscript={appendToDraft}
              onTerminalSubmit={props.onSubmit}
              readContext={props.readContext}
              refocusTerminal={() => inputEl?.focus()}
            />
            <button
              type="button"
              class="term-chat__send"
              data-testid="chat-send"
              onMouseDown={(e) => e.preventDefault()}
              onClick={sendLine}
              aria-label="Send to terminal"
            >
              <span class="term-chat__icon">send</span>
            </button>
          </div>
        </div>
      </Portal>
    </Show>
  );
}
