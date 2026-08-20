// Main pane: renders the active session's terminal (from URL params),
// or an appropriate view for /swarm, /queue, /inbox, /search, /file.
// On mobile when the sidebar drawer is open, a dim overlay covers this
// pane; tapping it closes the drawer (via closeSidebar).

import { useParams, useLocation, useNavigate } from "@solidjs/router";
import { createMemo, createEffect, on, untrack, Show, lazy } from "solid-js";
import { rootStore } from "../store/root.ts";
import { resolveSessionByFolder, resolveSessionByWorkspace, newestOpenSessionForFolderKey } from "../store/selectors.ts";
import { decodeFolderPath } from "../lib/terminalHref.ts";
import { rememberVisit } from "../lib/lastVisited.ts";
import {
  sessionsHydrated,
  terminalBootstrapStage,
  type TerminalBootstrapStage,
} from "../store/sync-bootstrap.ts";
import { installDeadRouteSafetyNet } from "../lib/deadRouteSafetyNet.ts";
import { installStuckTerminalWatcher } from "../lib/stuckTerminal.ts";
import { consumeBootRestore } from "../lib/bootRestore.ts";
import { folderKeyOf } from "../lib/folderKey.ts";
import { signal } from "@roost/shared/diag";
import { TerminalDeck } from "./TerminalDeck.tsx";
import { Button } from "./Settings/md/Button.tsx";
import { uiStore, closeSidebar } from "../store/uiStore.ts";
import { isCompact } from "../lib/windowSizeClass.ts";
import type { Session } from "@roost/shared/wire";
import {
  TerminalLoadingNotice,
  type TerminalLoadingStage,
} from "./TerminalOfflineNotice.tsx";

// Code-split boundary (ts-no-dynamic-import exception): solid `lazy` is the
// bundler's split mechanism — the file viewer (+ syntaxLite) loads only when a
// /file/ route is actually visited (perf sweep C2.1).
const FileViewerSheet = lazy(() =>
  import("./FileViewerSheet.tsx").then((m) => ({ default: m.FileViewerSheet })),
);

interface BootstrapLoadingCopy {
  stage: Exclude<TerminalBootstrapStage, "ready"> & TerminalLoadingStage;
  title: string;
  detail: string;
}

function terminalBootstrapCopy(stage: TerminalBootstrapStage): BootstrapLoadingCopy | null {
  switch (stage) {
    case "identity":
      return {
        stage,
        title: "Connecting to coordinator",
        detail: "Waiting for coordinator identity.",
      };
    case "authorization":
      return {
        stage,
        title: "Authorizing this browser",
        detail: "Waiting for the coordinator to trust this browser.",
      };
    case "sync":
      return {
        stage,
        title: "Opening terminal connection",
        detail: "Coordinator reached; waiting for the live terminal channel.",
      };
    case "sessions":
      return {
        stage,
        title: "Loading terminal sessions",
        detail: "Live terminal channel opened; waiting for the session list.",
      };
    case "ready":
      return null;
  }
  const unreachable: never = stage;
  return unreachable;
}

export function MainPane() {
  const params = useParams<{
    sessionId?: string;
    workspaceId?: string;
    channelId?: string;
    workerFp?: string;
    folderPath?: string;
    path?: string;
  }>();
  const location = useLocation();

  const navigate = useNavigate();


  const activeSession = createMemo((): Session | null => {
    // /t/:workerFp/*folderPath — stable route, resolves by (server, spawn
    // folder) to the newest live session in that folder (null → safety net).
    if (params.workerFp && params.folderPath !== undefined) {
      return resolveSessionByFolder(params.workerFp, decodeFolderPath(params.folderPath, params.workerFp));
    }
    // /s/:sessionId — primary route, resolves by id directly.
    if (params.sessionId) {
      return rootStore.sessions[params.sessionId] ?? null;
    }
    // /w/:workspaceId — resolve to the workspace's newest open session (null →
    // safety net bounces home). Guard !channelId so the legacy
    // /w/:id/t/:channelId form still uses the channel branch below.
    if (params.workspaceId && !params.channelId) {
      return resolveSessionByWorkspace(params.workspaceId);
    }
    // Legacy /w/:workspaceId/t/:channelId — fall back to channel-id lookup.
    const channelId = params.channelId;
    if (!channelId) return null;
    const chNum = parseInt(channelId, 10);
    if (isNaN(chNum)) return null;
    return (
      Object.values(rootStore.sessions).find((s) => s.channel === chNum) ?? null
    );
  });

  // Only an OPEN session can be the live terminal. A closed/killed session may
  // linger in the store (status "closed") so activeSession still resolves it,
  // but TerminalDeck renders only open sessions — leaving a blank pane while
  // the TabBar still lists the rest. Route off it instead.
  const onTerminalRoute = createMemo(() =>
    !!(params.sessionId || params.channelId || params.workspaceId || (params.workerFp && params.folderPath !== undefined)),
  );
  const activeOpenSession = createMemo(() => {
    const s = activeSession();
    return s && s.status === "open" ? s : null;
  });


  // Dead-route safety net: never strand the user on a blank pane at a terminal
  // route with no open session. A live session's URL-resolution can blip to
  // null for a tick; the installer's grace window + re-check absorbs that (no
  // bounce — the "randomly bounced to Home" bug), and only a durably-gone
  // terminal navigates — to the newest open sibling in the same folder, else
  // Home. Every real bounce emits a Tier-1 diag signal. See deadRouteSafetyNet.
  installDeadRouteSafetyNet({
    onTerminalRoute,
    activeOpenSession,
    hydrated: sessionsHydrated,
    navigate,
    bounceTarget: (lastOpen) => {
      if (lastOpen) {
        const sib = newestOpenSessionForFolderKey(folderKeyOf(lastOpen), lastOpen.id);
        if (sib) return `/s/${sib.id}`;
      }
      return "/";
    },
    onBounce: (target, lastOpen) =>
      signal("nav.safety_net_redirect", {
        sid: lastOpen?.id ?? "",
        reason: lastOpen ? "gone" : "stale-deeplink",
        target,
      }),
  });

  // Never leave a terminal route as a blank/black pane the user can't escape.
  // When the URL resolves to no open session AND bootstrap is genuinely stuck —
  // coord unreachable (a fresh load mid-restart) or this browser unpaired — the
  // safety net above can't fire (it waits on hydration that never lands). Flip a
  // flag so the render shows an actionable card with a Go-home escape instead.
  // Debounced so a healthy load's fast hydration never flashes it; the
  // hydrated-but-gone case stays with the safety net's auto-bounce.
  const stuckKind = installStuckTerminalWatcher({
    onTerminalRoute,
    hasOpenSession: () => activeOpenSession() != null,
    hydrated: sessionsHydrated,
    unauthorized: () => rootStore.browser_unauthorized,
  });

  const bootstrapLoading = createMemo(() => {
    if (
      !onTerminalRoute()
      || activeOpenSession() !== null
      || sessionsHydrated()
      || stuckKind() === "unpaired"
    ) return null;
    return terminalBootstrapCopy(terminalBootstrapStage());
  });

  // Remember where you were (browser-local): boot restores into your last
  // terminal, and each folder reopens its last-viewed tab. Only records a LIVE
  // terminal so a dead route never overwrites a good memory.
  // Re-keyed (perf sweep C1.6): tracking the whole session object meant a
  // localStorage JSON round-trip per update of the viewed session. Key on id +
  // folder only — the two fields rememberVisit actually persists.
  createEffect(on(
    () => { const s = activeOpenSession(); return s ? `${s.id}\u0000${s.spawn_cwd ?? s.cwd}` : null; },
    () => { const s = untrack(activeOpenSession); if (s) rememberVisit(s); },
  ));

  const isFileView = createMemo(() => location.pathname.startsWith("/file/"));
  const isSearch = createMemo(() => location.pathname.startsWith("/search"));
  // File viewer / search render as overlays ABOVE the always-mounted deck host
  // below; while active the host is visibility-flipped, never unmounted.
  const overlayActive = () => isFileView() || isSearch();

  const isMobile = isCompact;

  return (
    <div style={{ flex: 1, display: "flex", "flex-direction": "column", overflow: "hidden", position: "relative" }}>
      {/* Mobile dim overlay — only active when sidebar drawer is open.
          Rendered here (z-index 49, same as AppShell overlay) so tapping
          anywhere on the main pane collapses the drawer. AppShell's overlay
          is the primary handler; this one provides pointer-events:none
          guard so main content stays non-interactive behind the drawer. */}
      <Show when={isMobile() && uiStore.sidebarOpen}>
        <div
          data-testid="main-pane-sidebar-dim"
          onClick={closeSidebar}
          style={{
            position: "absolute",
            inset: "0",
            background: "transparent",
            "z-index": "48",
            cursor: "pointer",
          }}
          aria-hidden="true"
        />
      </Show>

      <Show when={isFileView()}>
        <FileViewerSheet />
      </Show>

      <Show when={isSearch()}>
        <div style={{ padding: "20px", color: "var(--text-hi)" }}>
          {/* TODO R4.3: Global search */}
          <p>Search</p>
        </div>
      </Show>

      {/* Persistent terminal deck — mounts every open terminal once and keeps
          it alive across ALL navigation, including /file/… and /search: those
          overlays only flip this host's visibility (layout is preserved, so
          park geometry stays truthful and warmSessionIds + every renderer
          survive the trip; returning is a pure restyle — no remount, no WASM
          init, no claim storm). Children opt back in with visibility:"inherit"
          (TerminalDeck termStyle) — a literal "visible" would bleed through
          the hidden host. FileViewerSheet is position:fixed z-index:50, above
          this un-z-indexed host; the search view is static text under a fully
          hidden, pointer-transparent host. */}
      <div
        style={{
          position: "absolute",
          inset: "0",
          display: "flex",
          "flex-direction": "column",
          visibility: overlayActive() ? "hidden" : "visible",
          "pointer-events": overlayActive() ? "none" : "auto",
        }}
        aria-hidden={overlayActive() ? "true" : undefined}
      >
        <TerminalDeck
          activeSessionId={activeOpenSession()?.id ?? null}
          surfaceVisible={!overlayActive()}
        />

        <Show when={bootstrapLoading()}>
          {(loading) => (
            <TerminalLoadingNotice
              stage={loading().stage}
              title={loading().title}
              detail={loading().detail}
              actions={stuckKind() === "connecting"
                ? (
                  <Button
                    variant="tonal"
                    data-testid="stuck-terminal-home"
                    onClick={() => { consumeBootRestore(); navigate("/"); }}
                  >
                    Go home
                  </Button>
                )
                : undefined}
            />
          )}
        </Show>

        {/* An unpaired browser cannot progress through bootstrap without user
            action, so replace progress immediately with pairing escapes. */}
        <Show when={stuckKind() === "unpaired"}>
          <div
            data-testid="stuck-terminal"
            data-kind="unpaired"
            style={{
              position: "absolute", inset: "0", "z-index": "20",
              display: "flex", "flex-direction": "column",
              "align-items": "center", "justify-content": "center",
              gap: "14px", padding: "32px", "text-align": "center",
              background: "var(--bg-base)",
            }}
          >
            <div style={{ "font-size": "15px", "font-weight": 600, color: "var(--text-hi)" }}>
              Browser not paired
            </div>
            <div style={{ "font-size": "13px", "line-height": 1.5, color: "var(--text-lo)", "max-width": "340px" }}>
              This browser isn't trusted by this coordinator yet. Pair it to open terminals.
            </div>
            <div style={{ display: "flex", gap: "8px", "margin-top": "4px" }}>
              <Button variant="filled" data-testid="stuck-terminal-pair" onClick={() => navigate("/pair")}>
                Pair this browser
              </Button>
              <Button variant="tonal" data-testid="stuck-terminal-home" onClick={() => { consumeBootRestore(); navigate("/"); }}>
                Go home
              </Button>
            </div>
          </div>
        </Show>
      </div>
    </div>
  );
}
