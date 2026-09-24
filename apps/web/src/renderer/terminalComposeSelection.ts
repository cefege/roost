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

interface TerminalComposeSelectionOptions {
  active: () => boolean;
  capture: () => TerminalSelectionGuard | undefined;
  dock: () => HTMLDivElement | undefined;
  input: () => HTMLTextAreaElement | undefined;
  afterInputMount: () => void;
}

/**
 * Owns the handoff between a terminal's native document Range and the
 * composer's private textarea selection.
 */
export function createTerminalComposeSelection(options: TerminalComposeSelectionOptions) {
  let terminalGuard: TerminalSelectionGuard | undefined;
  let selectionStart = 0;
  let selectionEnd = 0;
  let selectionDirection: "forward" | "backward" | "none" = "none";
  let composerSelectionActive = false;
  let composing = false;
  let stopInputEditCapture: (() => void) | undefined;
  let deferredKeyupRestore: {
    input: HTMLTextAreaElement;
    guard: TerminalSelectionGuard;
  } | undefined;
  let deferredLayoutGuard: TerminalSelectionGuard | undefined;
  let layoutRestoreVersion = 0;
  let pendingSuspendSelectionChange = false;

  const release = () => {
    layoutRestoreVersion += 1;
    deferredKeyupRestore = undefined;
    deferredLayoutGuard = undefined;
    pendingSuspendSelectionChange = false;
    terminalGuard?.release();
    terminalGuard = undefined;
    composerSelectionActive = false;
  };

  const capture = () => {
    const next = options.capture();
    if (!next) return;
    layoutRestoreVersion += 1;
    deferredKeyupRestore = undefined;
    deferredLayoutGuard = undefined;
    pendingSuspendSelectionChange = false;
    terminalGuard?.release();
    terminalGuard = next;
    composerSelectionActive = false;
  };

  const rememberComposerSelection = () => {
    const input = options.input();
    if (!input) return;
    selectionStart = input.selectionStart;
    selectionEnd = input.selectionEnd;
    selectionDirection = input.selectionDirection ?? "none";
  };

  const restore = (): boolean => {
    const guard = terminalGuard;
    if (!guard) return false;
    if (composerSelectionActive) rememberComposerSelection();
    if (guard.restore()) {
      composerSelectionActive = false;
      return true;
    }
    if (deferredLayoutGuard !== guard) release();
    return false;
  };

  const suspendGuard = (guard: TerminalSelectionGuard): boolean => {
    const selection = document.getSelection();
    const queuesEmptySelection = !!selection && !selection.isCollapsed;
    if (!guard.suspend()) return false;
    if (queuesEmptySelection) pendingSuspendSelectionChange = true;
    return true;
  };

  const activateComposerSelection = () => {
    const guard = terminalGuard;
    const input = options.input();
    if (!guard || !input || composerSelectionActive) return;
    const nativeSelection = input.ownerDocument.getSelection();
    const composerAlreadyOwnedSelection = !!nativeSelection && nativeSelection.isCollapsed;
    if (!suspendGuard(guard)) {
      release();
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
      input.setSelectionRange(selectionStart, selectionEnd, selectionDirection);
      composerSelectionActive = true;
    } catch {
      // A disabled/disconnected replacement cannot become an editing surface.
      release();
    }
  };

  // Programmatic focus (Playwright fill, browser autofill, accessibility
  // actions) has no pointerdown to capture the terminal range before Chromium
  // collapses it into the textarea. Retain the newest pane-owned range while it
  // is noncollapsed; a collapse outside this dock is a genuine abandonment.
  const onDocumentSelectionChange = () => {
    if (!options.active()) return;
    const selection = document.getSelection();
    if (pendingSuspendSelectionChange && selection?.rangeCount === 0) {
      pendingSuspendSelectionChange = false;
      return;
    }
    if (selection && !selection.isCollapsed) {
      // The keyup restore's own notification can arrive before Chromium's
      // following textarea collapse. Keep its one-shot token and original
      // guard until that collapse is observed.
      if (!deferredKeyupRestore) capture();
      return;
    }
    const deferred = deferredKeyupRestore;
    const input = options.input();
    if (
      deferred
      && input === deferred.input
      && document.activeElement === deferred.input
      && deferred.input.isConnected
      && !composing
      && terminalGuard === deferred.guard
    ) {
      window.setTimeout(() => {
        if (
          options.input() !== deferred.input
          || document.activeElement !== deferred.input
          || !deferred.input.isConnected
          || !options.active()
          || composing
          || terminalGuard !== deferred.guard
        ) return;
        if (restore() && deferredKeyupRestore === deferred) {
          deferredKeyupRestore = undefined;
        }
      }, 0);
      return;
    }
    if (!options.dock()?.contains(document.activeElement)) release();
  };
  document.addEventListener("selectionchange", onDocumentSelectionChange);

  const mountInput = (el: HTMLTextAreaElement) => {
    stopInputEditCapture?.();
    // A retained document Selection leaves the textarea focused but suppresses
    // Chromium's editable default. Suspend it before editing, then let one
    // versioned layout transaction restore after keyup's browser default.
    const onKeyDownCapture = () => {
      if (!composing) activateComposerSelection();
    };
    const onKeyUpCapture = () => {
      if (composing) return;
      const guard = terminalGuard;
      deferredKeyupRestore = guard ? { input: el, guard } : undefined;
      restoreAfterLayout();
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
      queueMicrotask(restore);
    };
    el.addEventListener("keydown", onKeyDownCapture, true);
    el.addEventListener("keyup", onKeyUpCapture, true);
    el.addEventListener("beforeinput", onBeforeInputCapture, true);
    el.addEventListener("compositionstart", onCompositionStartCapture, true);
    el.addEventListener("compositionend", onCompositionEndCapture, true);
    stopInputEditCapture = () => {
      if (deferredKeyupRestore?.input === el) deferredKeyupRestore = undefined;
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
      selectionStart = el.value.length;
      selectionEnd = el.value.length;
      selectionDirection = "none";
      options.afterInputMount();
    }, 0);
  };

  const restoreAfterLayout = () => {
    const guard = terminalGuard;
    if (!guard) return;
    const input = options.input();
    if (input) deferredKeyupRestore = { input, guard };
    const version = ++layoutRestoreVersion;
    deferredLayoutGuard = guard;
    window.setTimeout(() => {
      if (
        layoutRestoreVersion !== version
        || deferredLayoutGuard !== guard
        || terminalGuard !== guard
      ) return;
      deferredLayoutGuard = undefined;
      restore();
    }, 0);
  };

  return {
    capture,
    release,
    restore,
    restoreAfterLayout,
    hasGuard: () => terminalGuard !== undefined,
    isComposing: () => composing,
    markComposerSelectionActive: () => {
      composerSelectionActive = true;
    },
    rememberComposerSelection,
    rememberCaretAt: (position: number) => {
      selectionStart = position;
      selectionEnd = position;
      selectionDirection = "none";
    },
    prepareProgrammaticWrite: () => {
      if (!terminalGuard) capture();
      if (terminalGuard && !suspendGuard(terminalGuard)) release();
    },
    mountInput,
    dispose: () => {
      document.removeEventListener("selectionchange", onDocumentSelectionChange);
      stopInputEditCapture?.();
      release();
    },
  };
}
