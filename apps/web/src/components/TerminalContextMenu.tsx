// Right-click context menu over the terminal pane. Adapted to v2 Session
// shape + terminalActions helpers.
// Desktop: floating positioned menu at cursor coordinates.
// Mobile (<768px): bottom action sheet that slides up from the screen edge.
//
// Items: Copy | Paste | New terminal | Attach file | Close terminal.
// Mounted alongside the Terminal component in MainPane.

import { Show, batch, createSignal, onCleanup, onMount, type JSX } from "solid-js";
import { Portal } from "solid-js/web";
import { useNavigate, useLocation } from "@solidjs/router";
import type { Session } from "@roost/shared/wire";
import { ctxMenuSurfaceStyle, CtxMenuItem, CtxMenuSeparator } from "./contextMenuPrimitives.tsx";
import { spawnShell, waitForSession, maybeAutoLaunchAgent } from "../lib/spawnSession.ts";
import { scheduleClose } from "../lib/pendingClose.ts";
import { closeLabelsFor, killAfterUndo, siblingOrHomeHref } from "../lib/closeSession.ts";
import { activeSessionForPath } from "../store/selectors.ts";
import { isCompact, isTouchDevice } from "../lib/windowSizeClass.ts";
import { isSpotlit, setSpotlightSessionId, clearSpotlight, visiblePaneCount } from "../store/spotlight.ts";

interface Props {
  session: Session;
  getContainer: () => HTMLDivElement | null;
  /** Opens the file picker and attaches the chosen file(s) — same path as
   *  drag-drop/paste. Lets touch devices attach without drag-and-drop. */
  onAttachFile: () => void;
  /** Sends clipboard text through the pane's current terminal mode. */
  onPasteText: (text: string) => void;

}

interface OpenState {
  x: number;
  y: number;
  selection: string;
}

// Bottom action-sheet on touch (phones + tablets), floating menu on mouse.
const isMobileViewport = () => isCompact() || isTouchDevice();

export function TerminalContextMenu(props: Props) {
  const navigate = useNavigate();
  const location = useLocation();
  const [open, setOpen] = createSignal<OpenState | null>(null);

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
    setOpen({ x: e.clientX, y: e.clientY, selection: selectionText });
  };
  const dismiss = () => setOpen(null);
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
    await navigator.clipboard.writeText(text).catch(() => { /* ignore */ });
    dismiss();
  };

  const doPaste = async () => {
    dismiss();
    const text = await navigator.clipboard.readText().catch(() => "");
    if (text) props.onPasteText(text);
  };


  const doNewTerminal = async () => {
    dismiss();
    try {
      const sessionId = await spawnShell(props.session.worker_fp, props.session.cwd);
      const s = await waitForSession(sessionId);
      maybeAutoLaunchAgent(sessionId);
      if (s) navigate(`/s/${s.id}`, { replace: false });
    } catch (e) {
      console.warn("[ctx] new terminal failed", e);
    }
  };

  // Open the picker WITHIN this tap (gesture) before dismissing, so iOS allows it.
  const doAttach = () => {
    props.onAttachFile();
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

  return (
    <Show when={open()}>
      {(s) => (
        // Portal to <body>: an ancestor <main> carries a `transform`, which
        // makes position:fixed resolve against <main>'s box (offset by the
        // sidebar width) instead of the viewport → menu lands ~320px off the
        // cursor. Portaling escapes the transformed containing block.
        <Portal>
        <Show
          when={!isMobileViewport()}
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
                style={sheetStyle()}
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
                <Show when={s().selection.length > 0}>
                  <SheetItem testid="ctx-copy-selection" onClick={() => doCopySelection(s().selection)}>
                    Copy
                  </SheetItem>
                </Show>
                <SheetItem testid="ctx-paste" onClick={doPaste}>
                  Paste
                </SheetItem>
                <SheetItem testid="ctx-new-terminal" onClick={doNewTerminal}>
                  New terminal
                </SheetItem>
                <SheetItem testid="ctx-attach" onClick={doAttach}>
                  Attach file
                </SheetItem>
                <SheetItem testid="ctx-close" onClick={doClose} danger>
                  Close terminal
                </SheetItem>
                <SheetItem testid="ctx-cancel" onClick={dismiss}>
                  Cancel
                </SheetItem>
              </div>
            </>
          }
        >
          {/* ── Desktop: floating positioned menu ──────────────────────── */}
          <div
            data-testid="terminal-context-menu"
            data-variant="floating"
            class="df-menu-enter"
            style={ctxMenuSurfaceStyle(s().x, s().y)}
            onClick={(e) => e.stopPropagation()}
          >
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
            <CtxMenuItem testid="ctx-close" onClick={doClose} danger>
              Close terminal
            </CtxMenuItem>
          </div>
        </Show>
        </Portal>
      )}
    </Show>
  );
}

// ─── mobile bottom sheet styles ─────────────────────────────────────────

function sheetStyle(): JSX.CSSProperties {
  return {
    position: "fixed",
    left: "0",
    right: "0",
    bottom: "0",
    "z-index": "41",
    background: "var(--bg-elev-2)",
    "border-top": "1px solid var(--border-strong)",
    "border-radius": "var(--md-shape-md) var(--md-shape-md) 0 0",
    "box-shadow": "0 -8px 32px rgba(0,0,0,0.45)",
    padding: "12px 0 calc(env(safe-area-inset-bottom, 0px) + 16px)",
    "user-select": "none",
    color: "var(--text-hi)",
    "font-size": "15px",
  };
}

function SheetItem(props: { testid: string; onClick: () => void; danger?: boolean; children: JSX.Element }) {
  return (
    <div
      data-testid={props.testid}
      role="menuitem"
      onClick={props.onClick}
      style={{
        padding: "14px 20px",
        cursor: "pointer",
        color: props.danger ? "var(--color-err)" : "var(--text-hi)",
        "min-height": "44px",
        display: "flex",
        "align-items": "center",
      }}
      onTouchStart={(e) => { (e.currentTarget as HTMLElement).style.background = "var(--border-strong)"; }}
      onTouchEnd={(e) => { (e.currentTarget as HTMLElement).style.background = "transparent"; }}
    >
      {props.children}
    </div>
  );
}
