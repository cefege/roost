// Owns the permanent terminal composer: drafts, submit, attachments, voice,
// native selection handoff, and compact/desktop layout.
// Pane composers claim native focus while editing; releasing that ownership
// synchronously blurs the field so terminal shortcuts can route trusted keys.
// AppShell reserves the viewport dock without resizing the terminal deck.

import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { MobileVoiceInput } from "./MobileVoiceInput.tsx";
import {
  createTerminalComposeSelection,
  type TerminalSelectionGuard,
} from "./TerminalComposeSelection.ts";
export type { TerminalSelectionGuard } from "./TerminalComposeSelection.ts";
import {
  getComposerDraft,
  saveComposerDraft,
  subscribeComposerDraft,
} from "../lib/composerDrafts.ts";
import { isCompact, isTouchDevice } from "../lib/windowSizeClass.ts";
import type { TerminalContext } from "../lib/keytermContext.ts";
import type { Session } from "@roost/shared/wire";
import type { InputAdmission } from "../ws/sync-outbound.ts";

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

type ComposeOwner = { sessionId: string; token: number; blurOwnedFocus: () => void };
let nextComposeOwnerToken = 0;
const [activeComposeOwner, setActiveComposeOwner] = createSignal<ComposeOwner | null>(null);
export const activeComposeSessionId = () => activeComposeOwner()?.sessionId ?? null;
export const releaseActiveComposeFocus = () => {
  const owner = activeComposeOwner();
  owner?.blurOwnedFocus();
  if (owner && activeComposeOwner() === owner) setActiveComposeOwner(null);
};

// Track the body-portaled viewport composer separately from focus ownership:
// responsive replacements can mount before the previous instance disposes.
let activeViewportToken: number | null = null;
export const [composerActive, setComposerActive] = createSignal(false);
export const [composerHeightPx, setComposerHeightPx] = createSignal(0);

export function TerminalComposeButton(props: Props) {
  // Capture once: deferred retention and cleanup must not read live props.
  const placement = props.placement;
  const viewportPlacement = placement === "viewport";
  const sessionId = props.session.id;
  const ownerToken = ++nextComposeOwnerToken;
  const [draft, setDraft] = createSignal(getComposerDraft(sessionId));
  const [pendingSubmission, setPendingSubmission] = createSignal<string | null>(null);
  const [submissionStatus, setSubmissionStatus] = createSignal<string | null>(null);
  const unsubscribeDraft = subscribeComposerDraft(sessionId, setDraft);
  let inputEl: HTMLTextAreaElement | undefined;
  let dockEl: HTMLDivElement | undefined;
  const ownsComposeFocus = () => activeComposeOwner()?.token === ownerToken;
  const claimComposeFocus = () => setActiveComposeOwner({
    sessionId, token: ownerToken, blurOwnedFocus: () => inputEl?.blur(),
  });
  const releaseComposeFocus = () => {
    if (ownsComposeFocus()) setActiveComposeOwner(null);
  };
  let dockObserver: ResizeObserver | undefined;
  let dockMutationObserver: MutationObserver | undefined;
  const terminalSelection = createTerminalComposeSelection({
    active: () => props.active,
    capture: () => props.captureTerminalSelection?.(),
    dock: () => dockEl,
    input: () => inputEl,
    afterInputMount: () => growAndFollow(),
  });

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
    if (!terminalSelection.hasGuard()) terminalSelection.capture();
    if (!viewportPlacement) claimComposeFocus();
  };

  let stopPointerReleaseWatch: (() => void) | undefined;
  const handleDockPointerDown = (event: PointerEvent) => {
    if (!props.active) return;
    stopPointerReleaseWatch?.();
    terminalSelection.capture();
    handleDockFocusIn();
    // Pane composers also retain their temporary focus claim through pointerup.
    // A viewport composer needs this watch only while guarding a Selection.
    if (viewportPlacement && !terminalSelection.hasGuard()) return;
    const pointerId = event.pointerId;
    const pointerTargetsInput = event.target === inputEl;
    const onPointerEnd = (endEvent: PointerEvent) => {
      if (endEvent.pointerId !== pointerId) return;
      stopPointerReleaseWatch?.();
      // Chromium preserves a document range through focus, then collapses it
      // before pointerup. Restore synchronously in pointerup capture, after that
      // native default and before the control's click can admit terminal input.
      if (pointerTargetsInput && document.activeElement === inputEl) {
        terminalSelection.markComposerSelectionActive();
      }
      terminalSelection.restoreAfterLayout();
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
    terminalSelection.restore();
    queueMicrotask(() => {
      if (dockEl?.contains(document.activeElement)) return;
      terminalSelection.release();
      if (!viewportPlacement && ownsComposeFocus()) releaseComposeFocus();
    });
  };

  createEffect(() => {
    if (props.active) return;
    stopPointerReleaseWatch?.();
    terminalSelection.release();
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
    terminalSelection.rememberCaretAt(inputEl.value.length);
  };
  const mountInput = (el: HTMLTextAreaElement) => {
    inputEl = el;
    terminalSelection.mountInput(el);
  };

  // Speech callbacks mutate a controlled textarea outside the browser's native
  // edit transaction. Yield the exact retained terminal Range before Solid
  // writes `value` or autogrow touches layout, then restore it after both. A mic
  // focus handoff may have dropped this component's guard while leaving the
  // native Range intact, so recapture that still-connected Range first.
  const writeSpeechDraft = (next: string) => {
    if (next === draft()) return;
    terminalSelection.prepareProgrammaticWrite();
    setDraft(next);
    queueMicrotask(() => {
      growAndFollow();
      terminalSelection.restoreAfterLayout();
    });
  };

  // Append finalized dictation with exactly one separating space and never a
  // leading one.
  const glued = (base: string, text: string) =>
    (base.length === 0 || base.endsWith(" ") ? base : `${base} `) + text;

  // Controls in the pill should not steal focus from an already-focused field.
  // If Escape/outside interaction blurred it, this does not reopen the keyboard.
  const keepKeyboard = (e: MouseEvent) => e.preventDefault();

  // Each engine update replaces the live tail; dictationBase = typed-at-open.
  // lastDictatedFinal snapshots settled phrases so an abandoned dictation can
  // keep real words while dropping the provisional tail.
  let dictationBase: string | null = null;
  let lastDictatedFinal: string | null = null;
  const showDictation = (text: string | null) => {
    if (text === null) {
      // Ended WITHOUT a commit (failure/empty/deactivation): keep base +
      // finalized words, drop the unfinalized tail — reverting to base alone
      // silently deleted dictated words, keeping draft() baked in a hypothesis
      // the engine never settled ("still recording" leaking into the next take).
      if (dictationBase === null) return;
      const kept = lastDictatedFinal ? glued(dictationBase, lastDictatedFinal) : dictationBase;
      dictationBase = null;
      lastDictatedFinal = null;
      writeSpeechDraft(kept);
      saveComposerDraft(sessionId, kept);
    } else {
      if (dictationBase === null) {
        dictationBase = draft();
        lastDictatedFinal = null;
      }
      writeSpeechDraft(text.length === 0 ? dictationBase : glued(dictationBase, text));
    }
  };
  // Explicit ✕: restore the pre-mic baseline — the only self-driven revert.
  const discardDictation = () => {
    const base = dictationBase ?? draft();
    dictationBase = null;
    lastDictatedFinal = null;
    writeSpeechDraft(base);
    saveComposerDraft(sessionId, base);
  };
  const commitDictation = (text: string) => {
    const base = dictationBase ?? draft();
    dictationBase = null;
    lastDictatedFinal = null;
    const committed = glued(base, text);
    writeSpeechDraft(committed);
    // Persist here, not via the reactive effect: an unmount-time commit runs
    // while that effect is already being torn down.
    saveComposerDraft(sessionId, committed);
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
    terminalSelection.release();
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
    // Unmount mid-dictation (tab switch, swap, pane close): keep base + settled
    // phrases, drop the provisional tail; replacements read this draft.
    if (dictationBase !== null) {
      const kept = lastDictatedFinal ? glued(dictationBase, lastDictatedFinal) : dictationBase;
      dictationBase = null;
      lastDictatedFinal = null;
      writeSpeechDraft(kept);
      saveComposerDraft(sessionId, kept);
    }
    unsubscribeDraft();
    stopPointerReleaseWatch?.();
    terminalSelection.dispose();
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
              terminalSelection.markComposerSelectionActive();
              terminalSelection.rememberComposerSelection();
              setDraft(e.currentTarget.value);
              autoGrow();
              if (!e.isComposing && !terminalSelection.isComposing()) {
                terminalSelection.restoreAfterLayout();
              }
            }}
            onKeyDown={(e) => {
              if (
                e.key === "Enter"
                && !e.shiftKey
                && !e.isComposing
                && !terminalSelection.isComposing()
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
            onFinalTranscript={(finalText) => { lastDictatedFinal = finalText; }}
            ownerId={sessionId}
            active={props.active && pendingSubmission() === null}
            onActiveChange={setDictating}
            onTranscript={commitDictation}
            onLiveTranscript={showDictation}
            onDiscard={discardDictation}
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
