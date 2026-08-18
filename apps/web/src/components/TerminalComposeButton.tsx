// TerminalComposeButton — a terminal session's permanent composer.
// Non-compact layouts keep the attachment, field, mic, and send controls in
// one row. Compact layouts use a stable two-row grid: a full-width field above
// attachment, mic, and send actions, so multiline text never loses action width.
// The dock follows --kb-offset without opening the soft keyboard on mount.
//
// The compact field grows within its full-width row while the action row stays
// stable. Plain Enter submits on non-touch devices; Shift+Enter and touch
// Return insert newlines. Sending clears the per-session retained draft and
// leaves the composer mounted. Escape only blurs the textarea.
//
// The inline MobileVoiceInput streams finalized speech into the same draft.
// The viewport composer owns activeComposeSessionId for its entire lifetime.
// Inline pane composers claim it while focus is inside their dock so terminal
// autofocus never competes with any of their controls.
// AppShell reserves the viewport composer's resting surface and turns measured
// excess height into a paint-only TerminalDeck shift, keeping grid dimensions
// stable.

import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { MobileVoiceInput } from "./MobileVoiceInput.tsx";
import {
  getComposerDraft,
  saveComposerDraft,
  subscribeComposerDraft,
} from "../lib/composerDrafts.ts";
import { isCompact, isTouchDevice } from "../lib/windowSizeClass.ts";
import type { TerminalContext } from "../lib/keytermContext.ts";
import type { Session } from "@roost/shared/wire";
import type { InputAdmission } from "../ws/sync-outbound.ts";

export interface TerminalSelectionGuard {
  /**
   * Temporarily yield the captured native range to the focused editor. This
   * removes only that exact range and retains it for a guarded restore.
   */
  suspend(): boolean;
  /** Restore the captured native range if its exact row identity is still live. */
  restore(): boolean;
  /** Drop every retained DOM reference without changing the current Selection. */
  release(): void;
}

interface Props {
  placement: "viewport" | "pane";
  session: Session;
  /** Whether this session currently owns a visible terminal surface. */
  active: boolean;
  /** Submits the draft through the pane's current terminal mode. */
  onSubmit: (text: string) => InputAdmission;
  /** Opens the native picker and attaches selected files to the terminal. */
  onAttachFiles: () => void;
  /** Live terminal text for Deepgram keyterm biasing; forwarded to the mic. */
  readContext?: () => TerminalContext;
  /**
   * Captures a pane-owned native terminal selection before an editable control
   * becomes the browser's active selection surface.
   */
  captureTerminalSelection?: () => TerminalSelectionGuard | undefined;
}

type ComposeOwner = { sessionId: string; token: number };
let nextComposeOwnerToken = 0;
const [activeComposeOwner, setActiveComposeOwner] = createSignal<ComposeOwner | null>(null);
export const activeComposeSessionId = () => activeComposeOwner()?.sessionId ?? null;
export const releaseActiveComposeFocus = () => setActiveComposeOwner(null);

// AppShell reserves only the body-portaled viewport composer. Keep its lifetime
// token independent from focus ownership: responsive swaps can mount the pane
// replacement before disposing the viewport instance, or vice versa.
let activeViewportToken: number | null = null;
export const [composerActive, setComposerActive] = createSignal(false);
export const [composerHeightPx, setComposerHeightPx] = createSignal(0);

export function TerminalComposeButton(props: Props) {
  // Captured once at body scope: retention and cleanup must not read props from
  // deferred callbacks.
  const placement = props.placement;
  const viewportPlacement = placement === "viewport";
  const sessionId = props.session.id;
  const ownerToken = ++nextComposeOwnerToken;
  const [draft, setDraft] = createSignal(getComposerDraft(sessionId));
  const [pendingSubmission, setPendingSubmission] = createSignal<string | null>(null);
  const [submissionStatus, setSubmissionStatus] = createSignal<string | null>(null);
  const unsubscribeDraft = subscribeComposerDraft(sessionId, setDraft);
  const ownsComposeFocus = () => activeComposeOwner()?.token === ownerToken;
  const claimComposeFocus = () => setActiveComposeOwner({ sessionId, token: ownerToken });
  const releaseComposeFocus = () => {
    if (ownsComposeFocus()) setActiveComposeOwner(null);
  };
  let inputEl: HTMLTextAreaElement | undefined;
  let dockEl: HTMLDivElement | undefined;
  let dockObserver: ResizeObserver | undefined;
  let dockMutationObserver: MutationObserver | undefined;
  let terminalSelectionGuard: TerminalSelectionGuard | undefined;
  let composerSelectionStart = 0;
  let composerSelectionEnd = 0;
  let composerSelectionDirection: "forward" | "backward" | "none" = "none";
  let composerSelectionActive = false;
  let composing = false;
  let stopInputEditCapture: (() => void) | undefined;

  const releaseTerminalSelectionGuard = () => {
    terminalSelectionGuard?.release();
    terminalSelectionGuard = undefined;
    composerSelectionActive = false;
  };
  const captureTerminalSelectionGuard = () => {
    const next = props.captureTerminalSelection?.();
    if (!next) return;
    terminalSelectionGuard?.release();
    terminalSelectionGuard = next;
    composerSelectionActive = false;
  };
  const rememberComposerSelection = () => {
    if (!inputEl) return;
    composerSelectionStart = inputEl.selectionStart;
    composerSelectionEnd = inputEl.selectionEnd;
    composerSelectionDirection = inputEl.selectionDirection ?? "none";
  };
  const restoreTerminalSelection = () => {
    const guard = terminalSelectionGuard;
    if (!guard) return;
    if (composerSelectionActive) rememberComposerSelection();
    if (guard.restore()) {
      composerSelectionActive = false;
      return;
    }
    releaseTerminalSelectionGuard();
  };
  const activateComposerSelection = () => {
    const guard = terminalSelectionGuard;
    if (!guard || !inputEl || composerSelectionActive) return;
    const nativeSelection = inputEl.ownerDocument.getSelection();
    const composerAlreadyOwnedSelection = !!nativeSelection && nativeSelection.isCollapsed;
    if (!guard.suspend()) {
      releaseTerminalSelectionGuard();
      return;
    }
    if (composerAlreadyOwnedSelection) {
      // Browser automation, soft keyboards, and paste can establish a real
      // textarea caret/selection before beforeinput without a keydown.
      composerSelectionActive = true;
      rememberComposerSelection();
      return;
    }
    try {
      inputEl.setSelectionRange(
        composerSelectionStart,
        composerSelectionEnd,
        composerSelectionDirection,
      );
      composerSelectionActive = true;
    } catch {
      // A disabled/disconnected replacement cannot become an editing surface.
      releaseTerminalSelectionGuard();
    }
  };
  // Programmatic focus (Playwright fill, browser autofill, accessibility
  // actions) has no pointerdown to capture the terminal range before Chromium
  // collapses it into the textarea. Retain the newest pane-owned range while it
  // is noncollapsed; a collapse outside this dock is a genuine abandonment.
  const onDocumentSelectionChange = () => {
    if (!props.active) return;
    const selection = document.getSelection();
    if (selection && !selection.isCollapsed) {
      captureTerminalSelectionGuard();
      return;
    }
    if (!dockEl?.contains(document.activeElement)) {
      releaseTerminalSelectionGuard();
    }
  };
  document.addEventListener("selectionchange", onDocumentSelectionChange);


  // Claim the viewport input surface synchronously with the component mount so
  // terminal autofocus cannot race the portaled composer into the DOM.
  if (viewportPlacement) {
    claimComposeFocus();
    activeViewportToken = ownerToken;
    setComposerActive(true);
  }
  const publishDockHeight = (el: HTMLDivElement) => {
    if (!viewportPlacement || dockEl !== el || activeViewportToken !== ownerToken) return;
    setComposerHeightPx(el.getBoundingClientRect().height);
  };
  const updatePaneConstraint = (el: HTMLDivElement) => {
    if (viewportPlacement || dockEl !== el || el.clientWidth === 0 || el.clientHeight === 0) return;
    const constrained = el.scrollWidth > el.clientWidth + 1 || el.scrollHeight > el.clientHeight + 1;
    el.toggleAttribute("data-size-constrained", constrained);
  };

  const mountDock = (el: HTMLDivElement) => {
    dockEl = el;
    dockObserver?.disconnect();
    dockMutationObserver?.disconnect();
    if (viewportPlacement) {
      dockObserver = new ResizeObserver(() => publishDockHeight(el));
      dockObserver.observe(el);
      publishDockHeight(el);
      // Solid may invoke the ref before the portaled node is connected.
      queueMicrotask(() => publishDockHeight(el));
      return;
    }
    const updateConstraint = () => updatePaneConstraint(el);
    dockObserver = new ResizeObserver(updateConstraint);
    dockObserver.observe(el);
    dockMutationObserver = new MutationObserver(updateConstraint);
    dockMutationObserver.observe(el, { childList: true, subtree: true, characterData: true });
    queueMicrotask(updateConstraint);
  };

  const stopObservingDock = () => {
    dockObserver?.disconnect();
    dockMutationObserver?.disconnect();
    dockObserver = undefined;
    dockMutationObserver = undefined;
    dockEl = undefined;
  };

  const handleDockFocusIn = () => {
    if (!props.active) return;
    if (!terminalSelectionGuard) captureTerminalSelectionGuard();
    if (!viewportPlacement) claimComposeFocus();
  };

  let stopPointerReleaseWatch: (() => void) | undefined;
  const handleDockPointerDown = (event: PointerEvent) => {
    if (!props.active) return;
    stopPointerReleaseWatch?.();
    captureTerminalSelectionGuard();
    handleDockFocusIn();
    // Pane composers also retain their temporary focus claim through pointerup.
    // A viewport composer needs this watch only while guarding a Selection.
    if (viewportPlacement && !terminalSelectionGuard) return;
    const pointerId = event.pointerId;
    const pointerTargetsInput = event.target === inputEl;
    const onPointerEnd = (endEvent: PointerEvent) => {
      if (endEvent.pointerId !== pointerId) return;
      stopPointerReleaseWatch?.();
      // Chromium preserves a document range through focus, then collapses it
      // before pointerup. Restore synchronously in pointerup capture, after that
      // native default and before the control's click can admit terminal input.
      if (pointerTargetsInput && document.activeElement === inputEl) {
        composerSelectionActive = true;
      }
      restoreTerminalSelection();
      releasePaneClaimIfUnfocused();
    };
    window.addEventListener("pointerup", onPointerEnd, true);
    window.addEventListener("pointercancel", onPointerEnd, true);
    stopPointerReleaseWatch = () => {
      window.removeEventListener("pointerup", onPointerEnd, true);
      window.removeEventListener("pointercancel", onPointerEnd, true);
      stopPointerReleaseWatch = undefined;
    };
  };

  const releasePaneClaimIfUnfocused = () => {
    // focusout follows the key default that moved focus. Return any suspended
    // range before dropping its guard; a new non-collapsed owner is never
    // overwritten because restore validates current Selection ownership.
    restoreTerminalSelection();
    queueMicrotask(() => {
      if (dockEl?.contains(document.activeElement)) return;
      releaseTerminalSelectionGuard();
      if (!viewportPlacement && ownsComposeFocus()) releaseComposeFocus();
    });
  };

  createEffect(() => {
    if (props.active) return;
    stopPointerReleaseWatch?.();
    releaseTerminalSelectionGuard();
    setDictating(false);
    releaseComposeFocus();
    const focused = document.activeElement;
    if (dockEl?.contains(focused) && focused instanceof HTMLElement) focused.blur();
  });


  // The textarea's height is imperative, so any programmatic draft change must
  // re-run it — onInput is not the only writer. `height:auto` makes a textarea
  // report its row height before scrollHeight is applied; CSS owns the cap.
  const autoGrow = () => {
    if (!inputEl) return;
    inputEl.style.height = "auto";
    inputEl.style.height = `${inputEl.scrollHeight}px`;
  };

  // A programmatic write must also keep the newest text visible past max-height.
  // Typing does not use this path because the caret already follows user input.
  const growAndFollow = () => {
    autoGrow();
    if (!inputEl) return;
    inputEl.scrollTop = inputEl.scrollHeight;
    // Dictation replaces a programmatic tail rather than the user's current
    // selection. Its next real edit must begin after that newest tail even while
    // the terminal Range owns document Selection.
    composerSelectionStart = inputEl.value.length;
    composerSelectionEnd = inputEl.value.length;
    composerSelectionDirection = "none";
  };
  const mountInput = (el: HTMLTextAreaElement) => {
    inputEl = el;
    stopInputEditCapture?.();
    // A retained document Selection leaves the textarea focused but suppresses
    // Chromium's editable default. Suspend that exact range and restore the
    // saved textarea caret in capture phase before key/beforeinput chooses its
    // edit range; onInput returns ownership to the terminal range.
    const onKeyDownCapture = () => {
      if (!composing) activateComposerSelection();
    };
    // Unlike a keydown microtask, keyup runs after the browser's editing
    // default. Input normally restores first; this is the no-input-key fallback.
    const onKeyUpCapture = () => {
      if (!composing) restoreTerminalSelection();
    };
    const onBeforeInputCapture = () => {
      if (!composing) activateComposerSelection();
    };
    const onCompositionStartCapture = () => {
      composing = true;
      activateComposerSelection();
    };
    const onCompositionEndCapture = () => {
      composing = false;
      queueMicrotask(restoreTerminalSelection);
    };
    el.addEventListener("keydown", onKeyDownCapture, true);
    el.addEventListener("keyup", onKeyUpCapture, true);
    el.addEventListener("beforeinput", onBeforeInputCapture, true);
    el.addEventListener("compositionstart", onCompositionStartCapture, true);
    el.addEventListener("compositionend", onCompositionEndCapture, true);
    stopInputEditCapture = () => {
      el.removeEventListener("keydown", onKeyDownCapture, true);
      el.removeEventListener("keyup", onKeyUpCapture, true);
      el.removeEventListener("beforeinput", onBeforeInputCapture, true);
      el.removeEventListener("compositionstart", onCompositionStartCapture, true);
      el.removeEventListener("compositionend", onCompositionEndCapture, true);
      stopInputEditCapture = undefined;
    };
    // DOM insertion completes after this turn. Restore retained-draft height and
    // the private textarea caret without focusing the field on startup.
    setTimeout(() => {
      if (!el.isConnected) return;
      el.setSelectionRange(el.value.length, el.value.length);
      composerSelectionStart = el.value.length;
      composerSelectionEnd = el.value.length;
      composerSelectionDirection = "none";
      growAndFollow();
    }, 0);
  };

  // Speech callbacks mutate a controlled textarea outside the browser's native
  // edit transaction. Yield the exact retained terminal Range before Solid
  // writes `value` or autogrow touches layout, then restore it after both. A mic
  // focus handoff may have dropped this component's guard while leaving the
  // native Range intact, so recapture that still-connected Range first.
  const writeSpeechDraft = (next: string) => {
    if (next === draft()) return;
    if (!terminalSelectionGuard) captureTerminalSelectionGuard();
    const guard = terminalSelectionGuard;
    if (guard && !guard.suspend()) releaseTerminalSelectionGuard();
    setDraft(next);
    queueMicrotask(() => {
      growAndFollow();
      restoreTerminalSelection();
    });
  };

  // Append finalized dictation with exactly one separating space and never a
  // leading one.
  const glued = (base: string, text: string) =>
    (base.length === 0 || base.endsWith(" ") ? base : `${base} `) + text;

  // Controls in the pill should not steal focus from an already-focused field.
  // If Escape/outside interaction blurred it, this does not reopen the keyboard.
  const keepKeyboard = (e: MouseEvent) => e.preventDefault();

  // Dictation streams into this field. Each engine update replaces its live
  // tail, so dictationBase holds whatever was typed when the mic opened.
  let dictationBase: string | null = null;
  const showDictation = (text: string | null) => {
    if (text === null) {
      if (dictationBase === null) return;
      const base = dictationBase;
      dictationBase = null;
      writeSpeechDraft(base);
      // Discard can run during child disposal after this component's reactive
      // writer is already being torn down. Persist and notify the replacement
      // synchronously so no provisional hypothesis survives the handoff.
      saveComposerDraft(sessionId, base);
    } else {
      dictationBase ??= draft();
      writeSpeechDraft(text.length === 0 ? dictationBase : glued(dictationBase, text));
    }
  };
  const commitDictation = (text: string) => {
    const base = dictationBase ?? draft();
    dictationBase = null;
    writeSpeechDraft(glued(base, text));
  };

  const [dictating, setDictating] = createSignal(false);

  // Submit and clear without changing dock ownership. Preserve textarea focus
  // only when it already had it, so send keeps an open keyboard open but does
  // not undo an intentional Escape/outside blur.
  const sendLine = () => {
    if (!props.active || pendingSubmission() !== null) return;
    const text = draft();
    const retainInputFocus = document.activeElement === inputEl;
    const admission = props.onSubmit(text);
    if (!admission.accepted) {
      setSubmissionStatus(admission.reason);
      return;
    }
    releaseTerminalSelectionGuard();
    setPendingSubmission(text);
    dictationBase = null;
    setDraft("");
    setSubmissionStatus(null);
    void admission.result.then((outcome) => {
      setPendingSubmission(null);
      if (outcome.status === "rejected") {
        if (draft() === "") setDraft(text);
        setSubmissionStatus(outcome.reason);
      } else if (outcome.status === "ambiguous") {
        setSubmissionStatus("Input may have been partially sent; it was not retried.");
      } else {
        setSubmissionStatus(null);
      }
      queueMicrotask(() => {
        autoGrow();
        if (!retainInputFocus || !inputEl) return;
        inputEl.focus({ preventScroll: true });
        inputEl.setSelectionRange(inputEl.value.length, inputEl.value.length);
      });
    });
  };

  // Write through every edit and follow the same session's brief responsive
  // replacement so a provisional dictation cannot survive in one instance.
  createEffect(() => saveComposerDraft(sessionId, draft()));

  onCleanup(() => {
    if (dictationBase !== null) {
      const base = dictationBase;
      dictationBase = null;
      setDraft(base);
      saveComposerDraft(sessionId, base);
    }
    unsubscribeDraft();
    stopPointerReleaseWatch?.();
    document.removeEventListener("selectionchange", onDocumentSelectionChange);
    stopInputEditCapture?.();
    releaseTerminalSelectionGuard();
    stopObservingDock();
    releaseComposeFocus();
    if (activeViewportToken === ownerToken) {
      activeViewportToken = null;
      setComposerHeightPx(0);
      setComposerActive(false);
    }
  });

  const dock = (
      <div
        class="term-chat__dock"
        data-testid="mobile-chat-input"
        data-open="true"
        data-placement={placement}
        data-active={props.active ? "true" : "false"}
        inert={!props.active ? true : undefined}
        aria-hidden={props.active ? undefined : "true"}
        ref={mountDock}
        onFocusIn={handleDockFocusIn}
        onPointerDown={handleDockPointerDown}
        onFocusOut={releasePaneClaimIfUnfocused}
      >
        <div class="term-chat__box" data-testid="chat-box" data-compact={isCompact() ? "true" : "false"}>
          <button
            type="button"
            class="term-chat__ctl term-chat__attach"
            data-testid="chat-attach"
            disabled={!props.active || pendingSubmission() !== null}
            onMouseDown={keepKeyboard}
            onClick={() => { if (props.active) props.onAttachFiles(); }}
            aria-label="Attach files"
            title="Attach files"
          >
            <span class="term-chat__icon">attach_file</span>
          </button>
          <textarea
            class="term-chat__input"
            data-testid="chat-input"
            disabled={!props.active || pendingSubmission() !== null}
            aria-label="Terminal input"
            rows={1}
            placeholder={dictating() ? "Listening…" : "Type terminal input…"}
            value={draft()}
            onInput={(e) => {
              // Input proves that the textarea selection was active. Snapshot its
              // post-edit caret before returning ownership to the terminal range.
              composerSelectionActive = true;
              rememberComposerSelection();
              setDraft(e.currentTarget.value);
              autoGrow();
              if (!e.isComposing && !composing) restoreTerminalSelection();
            }}
            onKeyDown={(e) => {
              if (
                e.key === "Enter"
                && !e.shiftKey
                && !e.isComposing
                && !composing
                && !isTouchDevice()
              ) {
                e.preventDefault();
                if (!dictating()) sendLine();
                return;
              }
              // Plain desktop Enter is inert while provisional dictation is
              // active. Shift+Enter and touch Return retain native newlines;
              // Escape only dismisses keyboard focus.
              if (e.key === "Escape") {
                e.preventDefault();
                e.currentTarget.blur();
              }
            }}
            ref={mountInput}
          />
          <MobileVoiceInput
            ownerId={sessionId}
            active={props.active && pendingSubmission() === null}
            onActiveChange={setDictating}
            onTranscript={commitDictation}
            onLiveTranscript={showDictation}
            readContext={props.readContext}
          />
          {/* Hidden while dictating: the inline mic's own discard action occupies
              the compact action row, preventing half-finalized speech from
              sending without changing the textarea's full-width row. */}
          <Show when={!dictating()}>
            <button
              type="button"
              class="term-chat__ctl term-chat__send"
              data-testid="chat-send"
              disabled={!props.active || pendingSubmission() !== null}
              onMouseDown={keepKeyboard}
              onClick={sendLine}
              aria-label="Send to terminal"
            >
              <span class="term-chat__icon">send</span>
            </button>
          </Show>
        </div>
        <Show when={submissionStatus()}>
          {(message) => <div class="term-chat__status" role="status">{message()}</div>}
        </Show>
      </div>
  );

  return viewportPlacement ? <Portal>{dock}</Portal> : dock;
}
