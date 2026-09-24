// Right-click context menu over the terminal pane. Adapted to v2 Session
// shape + terminalActions helpers.
// Compact viewports use a bottom action sheet; medium and desktop viewports
// always use a cursor-anchored floating menu, including touch-capable desktops.
//
// Items: Copy | Paste | New terminal | Attach file | terminal debugging
// (start/capture/stop) | Close terminal.
// Mounted alongside the Terminal component in MainPane.

import { copyToClipboard } from "../lib/clipboard.ts";
import { Show, batch, createSignal, onCleanup, onMount, type JSX } from "solid-js";
import { Portal } from "solid-js/web";
import { useNavigate, useLocation } from "@solidjs/router";
import type { Session } from "@roost/protocol/wire";
import {
  ctxMenuSurfaceStyle, CtxMenuItem, CtxMenuSeparator, focusMenuEdge,
  handleMenuKeyboardNavigation,
} from "./contextMenuPrimitives.tsx";
import { spawnSessionSibling } from "../lib/sessionSiblingAction.ts";
import { scheduleClose } from "../lib/pendingClose.ts";
import { closeLabelsFor, killAfterUndo, siblingOrHomeHref } from "../lib/closeSession.ts";
import { activeSessionForPath } from "../store/selectors.ts";
import { isCompact } from "../lib/windowSizeClass.ts";
import { isSpotlit, setSpotlightSessionId, clearSpotlight, visiblePaneCount } from "../store/spotlight.ts";
import { TerminalCaptureConsentDialog } from "./TerminalCaptureConsentDialog.tsx";
import { CaptureStateRow } from "./TerminalCaptureStateRow.tsx";
import { TerminalSheetItem } from "./TerminalSheetItem.tsx";
import { createTerminalCaptureMenuController } from "./terminalCaptureMenuController.ts";

interface Props {
  session: Session;
  getContainer: () => HTMLDivElement | null;
  /** Opens the file picker and attaches the chosen file(s) — same path as
   *  drag-drop/paste. Lets touch devices attach without drag-and-drop. */
  onAttachFile: () => void;
  /** Sends clipboard text through the pane's current terminal mode. */
  onPasteText: (text: string) => void;
  /** Safe explicit activation for touch and context-menu users. */
  onOpenLink: (anchor: HTMLAnchorElement) => void;
  describeLink: (anchor: HTMLAnchorElement) => string | null;

}

interface OpenState {
  x: number;
  y: number;
  selection: string;
  link: HTMLAnchorElement | null;
  linkTarget: string | null;
}

// Touch capability is not a layout mode: KDE and desktop browsers can expose
// a coarse pointer or touch points while a mouse right-click still needs a menu.
export function _terminalContextMenuUsesActionSheet(compact: boolean): boolean {
  return compact;
}

const usesActionSheet = () => _terminalContextMenuUsesActionSheet(isCompact());


export function TerminalContextMenu(props: Props) {
  const navigate = useNavigate();
  const location = useLocation();
  const [open, setOpen] = createSignal<OpenState | null>(null);
  const capture = createTerminalCaptureMenuController(() => props.session.id);
  // Roving focus for the floating menu, so a keyboard or a controller can reach
  // its items at all: CtxMenuItem is tabIndex=-1. The sheet branch's rows are
  // tabIndex=0 and are reached by ordinary directional travel instead.
  let menuElement: HTMLDivElement | undefined;
  let cancelPendingFocus: (() => void) | null = null;
  // Escape hands focus back to whatever held it when the menu opened (normally
  // the PTY textarea): dropping to <body> leaves spatialNavigation with no
  // origin, so the next directional press teleports across the page. Tab is
  // excluded — native sequential focus has already moved.
  let invoker: HTMLElement | null = null;
  const dismissAndRestoreFocus = () => {
    const target = invoker;
    dismiss();
    if (target?.isConnected) queueMicrotask(() => target.focus());
  };
  const onMenuKeyDown = (event: KeyboardEvent) =>
    handleMenuKeyboardNavigation(event, menuElement, dismissAndRestoreFocus, dismiss);

  const onCtx = (e: MouseEvent) => {
    const container = props.getContainer();
    // Right-click outside the terminal (e.g. a sidebar row) must close this
    // menu, not leave it lingering behind the menu that click opens.
    if (!container || !container.contains(e.target as Node)) { setOpen(null); return; }
    e.preventDefault();
    const sel = window.getSelection();
    const selectionText =
      sel && !sel.isCollapsed && container.contains(sel.anchorNode)
        ? sel.toString()
        : "";
    const link = (e.target as Element | null)
      ?.closest?.("a.wterm-link") as HTMLAnchorElement | null;
    const linkTarget = link ? props.describeLink(link) : null;
    setOpen({
      x: e.clientX,
      y: e.clientY,
      selection: selectionText,
      link: linkTarget ? link : null,
      linkTarget,
    });
    invoker = document.activeElement as HTMLElement | null;
    cancelPendingFocus?.();
    cancelPendingFocus = usesActionSheet()
      ? null
      : focusMenuEdge(() => menuElement, "first");
  };
  const dismiss = () => {
    cancelPendingFocus?.();
    cancelPendingFocus = null;
    setOpen(null);
  };
  const onEsc = (e: KeyboardEvent) => { if (e.key === "Escape") dismiss(); };

  onMount(() => {
    document.addEventListener("contextmenu", onCtx);
    document.addEventListener("click", dismiss);
    document.addEventListener("keydown", onEsc);
    onCleanup(() => {
      document.removeEventListener("contextmenu", onCtx);
      document.removeEventListener("click", dismiss);
      document.removeEventListener("keydown", onEsc);
    });
  });

  const doCopySelection = async (text: string) => {
    if (!text) return;
    // Denial is ignored — the menu still dismisses; the user can re-select.
    await copyToClipboard(text);
    dismiss();
  };

  const doPaste = async () => {
    dismiss();
    const text = await navigator.clipboard.readText().catch(() => "");
    if (text) props.onPasteText(text);
  };
  const doOpenLink = (anchor: HTMLAnchorElement) => {
    dismiss();
    props.onOpenLink(anchor);
  };


  const doNewTerminal = async () => {
    dismiss();
    await spawnSessionSibling(props.session, navigate);
  };

  // Open the picker WITHIN this tap (gesture) before dismissing, so iOS allows it.
  const doAttach = () => {
    props.onAttachFile();
    dismiss();
  };

  // The recorder freezes the on-screen evidence inside these calls, BEFORE
  // dismiss() moves focus: dismissal can change reader holds and repaint, and a
  // capture must own what the operator is looking at, not what follows it.
  const doStartDebugging = () => {
    capture.requestStartDebugging();
    dismiss();
  };

  const doCaptureDiagnostic = () => {
    capture.requestCapture();
    dismiss();
  };

  const doStopDebugging = () => {
    capture.requestStopDebugging();
    dismiss();
  };

  // Unified with the tab-✕ / sidebar close: no confirm dialog, just the 5s
  // soft-close. Disappears this frame; when this menu targets the viewed
  // session, land on a sibling/Home now and let Undo navigate back.
  const doClose = () => {
    dismiss();
    const s = props.session;
    const viewed = activeSessionForPath(location.pathname)?.id === s.id;
    batch(() => {
      scheduleClose(s.id, closeLabelsFor(s), killAfterUndo(s.id),
        viewed ? () => navigate(`/s/${s.id}`) : undefined);
      if (viewed) navigate(siblingOrHomeHref(s));
    });
  };

  // The menu dismisses before the dialog opens, so Dialog's own opener capture
  // resolves to <body>. Hand the keyboard back to the pane's terminal input
  // instead of leaving the terminal unfocused after Cancel/Confirm.
  const returnFocusToTerminal = (event: Event) => {
    event.preventDefault();
    const container = props.getContainer();
    const keyboard = container?.querySelector<HTMLTextAreaElement>("textarea.terminal-input");
    (keyboard ?? container)?.focus({ preventScroll: true });
  };

  return (
    <>
    {/* Outside the open() Show: the confirmation outlives the menu that raised
        it, because the menu dismisses before consent is given. */}
    <TerminalCaptureConsentDialog
      kind={capture.consentKind()}
      onConfirm={capture.confirmConsent}
      onCancel={capture.cancelConsent}
      onCloseAutoFocus={returnFocusToTerminal}
    />
    <Show when={open()}>
      {(s) => (
        // Portal to <body>: an ancestor <main> carries a `transform`, which
        // makes position:fixed resolve against <main>'s box (offset by the
        // sidebar width) instead of the viewport → menu lands ~320px off the
        // cursor. Portaling escapes the transformed containing block.
        <Portal>
        <Show
          when={!usesActionSheet()}
          fallback={
            // ── Mobile: bottom action sheet ──────────────────────────────
            <>
              {/* Backdrop */}
              <div
                data-testid="terminal-context-sheet-backdrop"
                style={{
                  position: "fixed",
                  inset: "0",
                  background: "color-mix(in srgb, var(--md-scrim) 50%, transparent)",
                  "z-index": "40",
                }}
                onClick={dismiss}
                aria-hidden="true"
              />
              {/* Sheet */}
              <div
                data-testid="terminal-context-menu"
                data-variant="sheet"
                style={_terminalActionSheetStyle()}
                onClick={(e) => e.stopPropagation()}
              >
                {/* Drag handle visual */}
                <div style={{
                  width: "40px",
                  height: "4px",
                  background: "var(--border-strong)",
                  "border-radius": "2px",
                  margin: "0 auto 12px",
                }} />
                <Show when={s().link}>
                  {(link) => (
                    <TerminalSheetItem testid="ctx-open-link" onClick={() => doOpenLink(link())}>
                      <span style={{ overflow: "hidden", "text-overflow": "ellipsis", "white-space": "nowrap" }}>
                        Open link · {s().linkTarget}
                      </span>
                    </TerminalSheetItem>
                  )}
                </Show>
                <Show when={s().selection.length > 0}>
                  <TerminalSheetItem testid="ctx-copy-selection" onClick={() => doCopySelection(s().selection)}>
                    Copy
                  </TerminalSheetItem>
                </Show>
                <TerminalSheetItem testid="ctx-paste" onClick={doPaste}>
                  Paste
                </TerminalSheetItem>
                <TerminalSheetItem testid="ctx-new-terminal" onClick={doNewTerminal}>
                  New terminal
                </TerminalSheetItem>
                <TerminalSheetItem testid="ctx-attach" onClick={doAttach}>
                  Attach file
                </TerminalSheetItem>
                <CaptureStateRow state={capture.captureState()} />
                <TerminalSheetItem testid="ctx-debug-start" disabled={capture.startDisabled()} onClick={doStartDebugging}>
                  Start terminal debugging
                </TerminalSheetItem>
                <TerminalSheetItem testid="ctx-capture-diagnostics" disabled={capture.captureDisabled()} onClick={doCaptureDiagnostic}>
                  Capture terminal diagnostic
                </TerminalSheetItem>
                <TerminalSheetItem testid="ctx-debug-stop" disabled={capture.stopDisabled()} onClick={doStopDebugging}>
                  Stop terminal debugging
                </TerminalSheetItem>
                <TerminalSheetItem testid="ctx-close" onClick={doClose} danger>
                  Close terminal
                </TerminalSheetItem>
                <TerminalSheetItem testid="ctx-cancel" onClick={dismiss}>
                  Cancel
                </TerminalSheetItem>
              </div>
            </>
          }
        >
          {/* ── Desktop: floating positioned menu ──────────────────────── */}
          <div
            ref={menuElement}
            role="menu"
            aria-label="Terminal pane actions"
            data-testid="terminal-context-menu"
            data-variant="floating"
            class="df-menu-enter"
            style={ctxMenuSurfaceStyle(s().x, s().y)}
            onClick={(e) => e.stopPropagation()}
            onKeyDown={onMenuKeyDown}
          >
            <Show when={s().link}>
              {(link) => (
                <>
                  <CtxMenuItem
                    testid="ctx-open-link"
                    title={s().linkTarget ?? undefined}
                    onClick={() => doOpenLink(link())}
                  >
                    <span style={{ display: "block", overflow: "hidden", "text-overflow": "ellipsis", "white-space": "nowrap" }}>
                      Open link · {s().linkTarget}
                    </span>
                  </CtxMenuItem>
                  <CtxMenuSeparator />
                </>
              )}
            </Show>
            <Show when={s().selection.length > 0}>
              <CtxMenuItem testid="ctx-copy-selection" onClick={() => doCopySelection(s().selection)}>
                Copy
              </CtxMenuItem>
              <CtxMenuSeparator />
            </Show>
            <CtxMenuItem testid="ctx-paste" onClick={doPaste}>
              Paste
            </CtxMenuItem>
            <CtxMenuSeparator />
            <CtxMenuItem testid="ctx-new-terminal" onClick={doNewTerminal}>
              New terminal
            </CtxMenuItem>
            <CtxMenuItem testid="ctx-attach" onClick={doAttach}>
              Attach file
            </CtxMenuItem>
            <Show when={visiblePaneCount() >= 2 && !isSpotlit(props.session.id)}>
              <CtxMenuSeparator />
              <CtxMenuItem testid="ctx-spotlight" onClick={() => { setSpotlightSessionId(props.session.id); dismiss(); }}>
                Bring to front
              </CtxMenuItem>
            </Show>
            <Show when={isSpotlit(props.session.id)}>
              <CtxMenuSeparator />
              <CtxMenuItem testid="ctx-unspotlight" onClick={() => { clearSpotlight(); dismiss(); }}>
                Push back
              </CtxMenuItem>
            </Show>
            <CtxMenuSeparator />
            <CaptureStateRow state={capture.captureState()} />
            <CtxMenuItem testid="ctx-debug-start" disabled={capture.startDisabled()} onClick={doStartDebugging}>
              Start terminal debugging
            </CtxMenuItem>
            <CtxMenuItem testid="ctx-capture-diagnostics" disabled={capture.captureDisabled()} onClick={doCaptureDiagnostic}>
              Capture terminal diagnostic
            </CtxMenuItem>
            <CtxMenuItem testid="ctx-debug-stop" disabled={capture.stopDisabled()} onClick={doStopDebugging}>
              Stop terminal debugging
            </CtxMenuItem>
            <CtxMenuSeparator />
            <CtxMenuItem testid="ctx-close" onClick={doClose} danger>
              Close terminal
            </CtxMenuItem>
          </div>
        </Show>
        </Portal>
      )}
    </Show>
    </>
  );
}

// ─── mobile bottom sheet styles ─────────────────────────────────────────

export function _terminalActionSheetStyle(): JSX.CSSProperties {
  return {
    position: "fixed",
    left: "0",
    right: "0",
    bottom: "max(var(--kb-offset), 0px)",
    "z-index": "41",
    "box-sizing": "border-box",
    background: "var(--bg-elev-2)",
    "border-top": "1px solid var(--border-strong)",
    "border-radius": "var(--md-shape-md) var(--md-shape-md) 0 0",
    "box-shadow": "0 -8px 32px rgba(0,0,0,0.45)",
    padding: "12px 0 calc(env(safe-area-inset-bottom, 0px) + 16px)",
    "max-height": "calc(100dvh - max(var(--kb-offset), 0px) - var(--md-space-4))",
    "overflow-y": "auto",
    "user-select": "none",
    color: "var(--text-hi)",
    "font-size": "15px",
  };
}

