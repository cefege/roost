// One row per terminal session in tree or flat sidebar density.
// Props: session, density. Navigates to /s/:sessionId on click.

import { A, useLocation, useNavigate } from "@solidjs/router";
import { batch, createMemo, createSignal, Show } from "solid-js";
import type { Session } from "@roost/shared/wire";
import { rootStore } from "../../store/root.ts";
import { workerOnline } from "../../store/sync.ts";
import { pushRecent } from "../../lib/sidebarRecent.ts";
import { scheduleClose } from "../../lib/pendingClose.ts";
import { closeLabelsFor, killAfterUndo, siblingOrHomeHref } from "../../lib/closeSession.ts";
import { activeSessionForPath } from "../../store/selectors.ts";
import { relTimeSince } from "../../lib/relTime.ts";
import { shortServerLabel } from "../../lib/sidebarFormat.ts";
import { sessionTitle } from "../../lib/sessionTitle.ts";
import { ViewersChip } from "./ViewersChip.tsx";
import { SessionRowContextMenu } from "./SessionRowContextMenu.tsx";
import { closeSidebar } from "../../store/uiStore.ts";
import { colorForFp } from "../../lib/fpColor.ts";
import { IconButton } from "../Settings/md/IconButton.tsx";
import "@material/web/ripple/ripple.js";
import { SessionRowFlat } from "./SessionRowFlat.tsx";
import { ROW_BASE, relTimeTickMs } from "./SessionRow.constants.ts";

interface SessionRowProps {
  session: Session;
  /** "full" = tree view (all chips); "flat" = calm recency list. */
  density?: "full" | "flat";
  /** Keyboard cursor highlight (flat mode). SEPARATE from data-selected
   *  (URL match) — see sidebarCursor.ts. */
  cursor?: boolean;
}

export { relTimeTickMs };

export function SessionRow(props: SessionRowProps) {
  const location = useLocation();
  const navigate = useNavigate();
  const session = () => props.session;
  const density = () => props.density ?? "full";
  // Server (Mac) identity for the flat list — plain text on the supporting
  // line (no color: arbitrary hues read as status). Owner prefix stripped.
  const serverLabel = createMemo(() =>
    shortServerLabel(rootStore.workers[session().worker_fp]?.label ?? session().worker_fp.slice(0, 6)),
  );
  // Laptop-icon tint: green = server reachable, red = down / not running.
  const serverOnline = createMemo(() => {
    const w = rootStore.workers[session().worker_fp];
    return w ? workerOnline(w) : false;
  });
  // An unreachable worker must render offline.
  const offline = createMemo(() => !serverOnline());
  // Terminal activity time for open sessions; lifecycle time for closed ones.
  const relTime = createMemo(() => {
    relTimeTickMs(); // 30s ticker so the label ages while nothing else changes
    const s = session();
    const ts = s.status === "open"
      ? (rootStore.last_activity[s.id] ?? s.created_at)
      : (s.closed_at ?? s.created_at);
    return relTimeSince(ts);
  });
  // Right-click context menu state.
  const [ctxMenu, setCtxMenu] = createSignal<{ x: number; y: number } | null>(null);

  // L11 / feedback_selected_means_url_match_not_has_children: exact
  // /t/<channel> match. pathname.includes(channel) was a substring
  // match — channel 1 matched /t/10, /t/11, /t/12; channel 2 matched
  // /t/20; clicking one session lit up every channel whose number was
  // a substring of the URL.
  const isActive = createMemo(() => {
    // Two URL shapes for a focused terminal:
    //   /s/<session_id>            — the post-sb6 default; the click
    //                                 handler navigates here.
    //   /w/<wsId>/t/<channel>      — legacy workspace-routed URL.
    // SessionRow lit nothing until now because we only matched the
    // second form, but the click handler emits the first → no row was
    // ever shown as "the one you're looking at".
    const path = location.pathname;
    const sid = session().id;
    if (path === `/s/${sid}` || path.startsWith(`/s/${sid}/`)) return true;
    const m = path.match(/\/t\/(\d+)/);
    return m !== null && m[1] === session().channel.toString();
  });



  // ── Swipe-to-close (touch only) ───────────────────────────────────────────
  // Drag a row left past the threshold to close it — Material swipe-to-dismiss,
  // routed through the same 5s-undo soft-close as the ✕. A red trash panel
  // grows from the right as you drag; release past threshold slides the row out
  // and closes, otherwise it springs back. Mouse never fires touch events, so
  // desktop is untouched.
  const SWIPE_CLOSE_THRESHOLD_PX = 96;
  const [swipeX, setSwipeX] = createSignal(0);
  const [swiping, setSwiping] = createSignal(false);
  let _touchStartX = 0;
  let _touchStartY = 0;
  let _swipeAxis: "none" | "x" | "y" = "none";
  let _swiped = false;
  function onTouchStart(e: TouchEvent) {
    const t = e.touches[0];
    if (!t) return;
    _touchStartX = t.clientX;
    _touchStartY = t.clientY;
    _swipeAxis = "none";
    _swiped = false;
    setSwiping(true); // finger-tracking: no transition
  }
  function onTouchMove(e: TouchEvent) {
    const t = e.touches[0];
    if (!t) return;
    const dx = t.clientX - _touchStartX;
    const dy = t.clientY - _touchStartY;
    if (_swipeAxis === "none") {
      if (Math.abs(dx) < 8 && Math.abs(dy) < 8) return;
      _swipeAxis = Math.abs(dx) > Math.abs(dy) ? "x" : "y";
    }
    if (_swipeAxis !== "x") return; // vertical → let the list scroll
    e.preventDefault(); // we own the gesture now
    const x = Math.max(-window.innerWidth, Math.min(0, dx)); // left only
    setSwipeX(x);
    if (x < -10) _swiped = true; // past a tap → suppress the click-nav
  }
  function onTouchEnd() {
    setSwiping(false); // re-enable transition for spring / slide-out
    if (swipeX() <= -SWIPE_CLOSE_THRESHOLD_PX) {
      setSwipeX(-window.innerWidth); // slide out, then close
      setTimeout(() => handleKill(), 180);
    } else {
      setSwipeX(0); // spring back
    }
  }

  function onClickRow(e: MouseEvent) {
    // Row is an <A> now. A swipe-release fires a click too — suppress the
    // anchor's native nav via preventDefault (solid-router's <A> bails when
    // defaultPrevented). Modifier/middle clicks fall through to the browser
    // (open-in-new-tab) untouched — <A> ignores them itself.
    if (_swiped) { _swiped = false; e.preventDefault(); return; }
    if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.altKey || e.shiftKey) return;
    const wid = session().workspace_id;
    if (wid) {
      void import("../../lib/lastWorkspace.ts").then((m) =>
        m.rememberLastWorkspace(session().worker_fp, wid as Parameters<typeof m.rememberLastWorkspace>[1])
      );
    }
    pushRecent(session().id);
    // Close the mobile drawer on the TAP, not via the route-change effect in
    // AppShell — re-tapping the session you're already on is a same-route
    // navigate (pathname unchanged), so that effect never fires and the drawer
    // stayed up covering the terminal ("can't get back to it"). No-op on
    // desktop (sidebarOpen is ignored there; sidebar is always visible).
    closeSidebar();
  }

  // Display name = the shared sessionTitle() (OSC title → cwd / shell), so the
  // sidebar row and the top TabBar read identically. See lib/sessionTitle.ts.
  const title = createMemo(() => sessionTitle(session()));
  // Avatar hue keyed on PROJECT (server + folder) — memoized so the swipe
  // transform re-render doesn't re-run the FNV hash every frame.
  const avatarBg = createMemo(
    () => `hsl(${colorForFp(`${session().worker_fp}|${session().cwd}`).hue} 48% 42%)`,
  );

  function handleKill(e?: MouseEvent) {
    e?.stopPropagation();
    setCtxMenu(null);
    // Soft-close: hide the row immediately (isPendingClose in the parent
    // group's filter) and fire the kill RPC after the 5s undo window.
    // Author 2026-06-17: 'like Gmail unsend — five seconds to restore.'
    // Closing the viewed session lands on a sibling/Home now; Undo goes back.
    const sid = session().id;
    const viewed = activeSessionForPath(location.pathname)?.id === sid;
    batch(() => {
      scheduleClose(sid, closeLabelsFor(session()), killAfterUndo(sid),
        viewed ? () => navigate(`/s/${sid}`) : undefined);
      if (viewed) navigate(siblingOrHomeHref(session()));
    });
  }

  return (
    <div class="df-row-swipe">
      <div class="df-row-del" aria-hidden="true" style={{ width: `${Math.max(0, -swipeX())}px` }}>
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor"
          stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2m3 0v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6" />
          <path d="M10 11v6M14 11v6" />
        </svg>
      </div>
    <A
      href={`/s/${session().id}`}
      data-testid="sidebar-session-row"
      data-session-id={session().id}
      data-worker-fp={session().worker_fp}
      data-status={session().status}
      data-selected={isActive() ? "focused" : ""}
      data-cursor={props.cursor ? "on" : undefined}
      data-density={density()}
      data-swiping={swiping() ? "1" : undefined}
      class="df-row"
      style={{
        ...(density() === "flat" ? { "padding-left": "12px" } : ROW_BASE),
        // Avatar color keyed on PROJECT (server + folder) — same folder/server
        // share a hue (visual grouping). Memoized (avatarBg) so swipe frames
        // don't re-hash. No ":" in the key so colorForFp hashes the whole thing.
        "--avatar-bg": avatarBg(),
        transform: `translateX(${swipeX()}px)`,
        transition: swiping()
          ? "none"
          : "transform var(--md-sys-motion-duration-short4, 200ms) var(--md-sys-motion-easing-emphasized-decelerate, cubic-bezier(0.05, 0.7, 0.1, 1))",
      }}
      onClick={onClickRow}
      onTouchStart={onTouchStart}
      onTouchMove={onTouchMove}
      onTouchEnd={onTouchEnd}
      onContextMenu={(e) => {
        e.preventDefault();
        e.stopPropagation();
        setCtxMenu({ x: e.clientX, y: e.clientY });
      }}
      title={`${title()} — ${session().cwd} — right-click for actions`}
    >
      <md-ripple />
      <span class="df-leading" aria-hidden="true">$</span>
      <Show
        when={density() === "flat"}
        fallback={
          <>
            <span class="df-label">{title()}</span>
            <Show when={offline()}>
              <span class="df-stage-text" data-stage="offline" data-testid={`session-offline-${session().id}`}>offline</span>
            </Show>
            <ViewersChip sessionId={session().id} />
          </>
        }
      >
        <SessionRowFlat
          session={session()}
          relTime={relTime()}
          serverOnline={serverOnline()}
          serverLabel={serverLabel()}
          offline={offline()}
        />
      </Show>
      <IconButton
        icon="close"
        label="Close pane"
        class="df-action df-action-always"
        data-testid={`session-close-${session().id}`}
        title="Close pane"
        onClick={(e: MouseEvent) => { e.stopPropagation(); e.preventDefault(); void handleKill(e); }}
        style={{ "--md-icon-button-icon-size": "14px" }}
      />
      <Show when={ctxMenu()}>
        {(pos) => (
          <SessionRowContextMenu
            session={session()}
            pos={pos()}
            onClose={() => setCtxMenu(null)}
            onDelete={(e) => void handleKill(e)}
          />
        )}
      </Show>
    </A>
    </div>
  );
}

