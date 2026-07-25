// RoostTerm — thin wrapper around @wterm/dom's WTerm that owns the focus
// dance. Single chokepoint per L11 row #13. Future CellTerminal.tsx
// simplifications cannot accidentally drop the dance without explicitly
// bypassing this wrapper (a visible, reviewable diff).
//
// Why a wrapper instead of inline code:
//   The focus dance has three load-bearing parts (blur-first,
//   FocusEvent dispatch, mousedown click-recapture) and every one of
//   them is non-obvious. A `/simplify` pass that touches CellTerminal.tsx
//   reflexively deletes "ad-hoc" looking event listeners. Hiding all
//   three behind `RoostTerm.attachFocusBridge()` keeps the surgery
//   localized: deleting the wrapper call lights up the L11 grep test
//   immediately ("focusedClass=false on mount"), and the wrapper
//   itself is named so that simplify agents read the file comment
//   before deleting anything.
//
// Why not patch @wterm/dom upstream:
//   We already vendor a patched WASM (apps/web/public/wterm-roost.wasm
//   per phase-pb9b) for the 10k scrollback bump, but the focus
//   behavior is in the JS layer, not the WASM. Forking the JS package
//   would mean shipping our own @wterm/dom build through the
//   apps/web/package.json — extra build steps + version drift cost
//   for every minor wterm bump. A 60-line wrapper is cheaper.

import { WTerm, type WTermOptions } from "@wterm/dom";
import { diag } from "@roost/shared/diag";

export interface RoostTermOptions extends WTermOptions {
  /** When true, calls forceFocus() immediately after init(). Default false.
   * Use only for the active pane in a deck — otherwise a background
   * session steals focus from the visible session. */
  autoFocus?: boolean;
  /** Input-host mode (CellTerminal): keep the WASM core (mode state, onData
   * replies) and the hidden textarea, but never paint wterm's own grid DOM.
   * That mirror grid is never shown, yet it grew to ~22k nodes per streamed
   * session and was re-dirtied on every write — the largest style/layout
   * flush surface behind the pane focus-switch stall. Also skips write()'s
   * per-chunk scrollHeight read (a forced layout per PTY chunk). */
  renderless?: boolean;
}

export class RoostTerm {
  readonly inner: WTerm;
  private readonly _container: HTMLElement;
  private _autoFocus: boolean;
  private _mousedownListener: ((ev: MouseEvent) => void) | null = null;
  private _clickListener: ((ev: MouseEvent) => void) | null = null;
  // Set true in destroy(). Any code that reads `.bridge` on a close/nav
  // race can hit an instance whose inner WTerm core was already torn down;
  // wterm's own `bridge` getter dereferences its null core and throws
  // `null is not an object (evaluating 'e.bridge')`. The flag lets the
  // getter short-circuit to null before touching inner.
  private _destroyed = false;
  private _renderless: boolean;

  constructor(container: HTMLElement, opts: RoostTermOptions = {}) {
    const { autoFocus, renderless, ...wopts } = opts;
    this._container = container;
    this._autoFocus = autoFocus === true;
    this._renderless = renderless === true;
    this.inner = new WTerm(container, wopts);
  }

  /** Forwards to WTerm.init() then installs the focus bridge. Returns
   * `this` so callers can chain. autoFocus fires forceFocus() after
   * init so a same-tick race between init + focus doesn't yield a
   * dark .focused class. */
  async init(): Promise<this> {
    // Stub the instance BEFORE inner.init() runs `_initialRender` — instance
    // properties shadow the prototype methods, so every internal
    // `this._scheduleRender()` / scroll read dispatches to these no-ops.
    if (this._renderless) {
      const inner = this.inner as unknown as {
        _initialRender: () => void;
        _doRender: () => void;
        _scheduleRender: () => void;
        _scrollToBottom: () => void;
        _isScrolledToBottom: () => boolean;
      };
      inner._initialRender = () => {};
      inner._doRender = () => {};
      inner._scheduleRender = () => {};
      inner._scrollToBottom = () => {};
      inner._isScrolledToBottom = () => true;
    }
    await this.inner.init();
    this._installFocusBridge();
    if (this._autoFocus) this.forceFocus();
    return this;
  }

  /** The three-piece focus dance. wterm's hidden textarea is at
   * left:-9729px and its .focused class is driven by the textarea's
   * `focus` event. Programmatic .focus() on an already-activeElement
   * textarea is a no-op → event never fires → onData never wires.
   * Blur first to guarantee the next .focus() dispatches an event;
   * manually dispatch a FocusEvent as belt-and-braces. Deliberately NO
   * synthetic bubbling `focusin`: nothing in-app listens for it, and
   * autofill/password-manager extensions run a full-page form scan on
   * every focusin (~100ms+ on a big deck, traced live) — the native
   * focus() already emits one real focusin; doubling it doubled the
   * focus-switch stall. */
  forceFocus(): void {
    const ta = this._container.querySelector("textarea") as HTMLTextAreaElement | null;
    try {
      if (ta && document.activeElement === ta) ta.blur();
      this.inner.focus();
      if (ta) {
        ta.dispatchEvent(new FocusEvent("focus", { bubbles: true }));
      }
    } catch { /* ignore */ }
    // landed=false here is the "can't input" smoking gun: focus didn't reach
    // the textarea → onData never fires → no bytes.up_send → keystrokes vanish
    // (the cell-phase-3b cell-mode focus bug). Grep focus.force landed:false.
    diag("focus.force", {
      landed: !!ta && document.activeElement === ta,
      has_textarea: !!ta,
    });
  }

  /** Clicks on visible cells land on row spans, not the off-screen
   * textarea. Without this, clicking the terminal doesn't refocus it
   * and the user types into whichever pane stole focus last. */
  private _installFocusBridge(): void {
    const listener = (ev: MouseEvent): void => {
      // Left-click only. Right-click (button 2) opens the terminal context
      // menu; forceFocus would collapse the marked selection, and its
      // microtask runs before `contextmenu` reads getSelection() → the
      // "Copy selection" item never sees the text.
      if (ev.button !== 0) return;
      const t = ev.target as HTMLElement | null;
      if (t?.closest("button, input, textarea, a")) return;
      // queueMicrotask so the browser's own focus shift from the
      // click lands first, then we force the textarea + event.
      queueMicrotask(() => this.forceFocus());
    };
    this._mousedownListener = listener;
    this._container.addEventListener("mousedown", listener);
    // The mousedown microtask above runs BEFORE a real (trusted) click's native
    // focus settles — clicking the non-focusable cell spans moves focus to
    // <body> AFTER us, so the textarea never holds focus and keystrokes vanish
    // (mic/buttons still work — programmatic). The 'click' event fires after
    // native focus settles, so re-grab there — unless the user selected text.
    const clickListener = (ev: MouseEvent): void => {
      if (ev.button !== 0) return;
      const t = ev.target as HTMLElement | null;
      if (t?.closest("button, input, textarea, a")) return;
      const sel = this._container.ownerDocument.getSelection();
      if (sel && !sel.isCollapsed) return; // preserve a text selection for copy
      this.forceFocus();
    };
    this._clickListener = clickListener;
    this._container.addEventListener("click", clickListener);
  }

  destroy(): void {
    this._destroyed = true;
    if (this._mousedownListener) {
      this._container.removeEventListener("mousedown", this._mousedownListener);
      this._mousedownListener = null;
    }
    if (this._clickListener) {
      this._container.removeEventListener("click", this._clickListener);
      this._clickListener = null;
    }
    this.inner.destroy();
  }

  get destroyed(): boolean { return this._destroyed; }

  // Pass-through accessors for the WTerm surface CellTerminal.tsx uses.
  // Adding new pass-throughs is cheap; the wrapper exists to own
  // focus, not to abstract wterm.
  get cols(): number { return this.inner.cols; }
  get rows(): number { return this.inner.rows; }
  get bridge(): WTerm["bridge"] | null { return this._destroyed ? null : this.inner.bridge; }
  get renderer(): WTerm["renderer"] { return (this.inner as unknown as { renderer: WTerm["renderer"] }).renderer; }
  get element(): HTMLElement { return this.inner.element; }
  get onData(): WTerm["onData"] { return this.inner.onData; }
  set onData(fn: WTerm["onData"]) { this.inner.onData = fn; }
  get onTitle(): WTerm["onTitle"] { return this.inner.onTitle; }
  set onTitle(fn: WTerm["onTitle"]) { this.inner.onTitle = fn; }
  get onResize(): WTerm["onResize"] { return this.inner.onResize; }
  set onResize(fn: WTerm["onResize"]) { this.inner.onResize = fn; }
  write(data: string | Uint8Array): void { this.inner.write(data); }
  resize(cols: number, rows: number): void { this.inner.resize(cols, rows); }
  focus(): void { this.forceFocus(); }
}
