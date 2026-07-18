// Route definitions. URL is source of truth for nav state (R0.18).
// Consumed by App.tsx's <Router> via @solidjs/router RouteDefinition.

export const ROUTES = {
  ROOT: "/",
  // /s/:sessionId — the new primary terminal route. Replaces the old
  // /w/:workspaceId/t/:channelId form. Workspaces no longer drive nav
  // since they're auto-grouped from cwd.
  SESSION: "/s/:sessionId",
  // /t/:workerFp/*folderPath — STABLE terminal URL keyed on server + spawn
  // folder (not the ephemeral session id). Resolves to the live session spawned
  // in that folder (resolveSessionByFolder, tiebreak newest). Survives a
  // session death+respawn in the same folder — the bookmark keeps working.
  // Mirrors the FILE route's (workerFp, splat path) shape.
  TERMINAL_BY_FOLDER: "/t/:workerFp/*folderPath",
  // Legacy /w/:workspaceId/t/:channelId — kept so old bookmarks redirect
  // gracefully. MainPane resolves by channelId.
  WORKSPACE: "/w/:workspaceId",
  WORKSPACE_TERMINAL: "/w/:workspaceId/t/:channelId",
  // phase-m3d: /swarm /queue /inbox routes removed (Author 2026-06-18 —
  // "those were basically the statuses of cloud code; there's no point
  // of filtering by each. There should be a list."). All sessions
  // render in the single AllView list. Per-row status chips
  // (running/idle/needs-input) survive — only the top-level
  // filter-routes are gone.
  SETTINGS: "/settings/:pane?",
  // /pair — always-reachable browser-pairing surface. WorkspaceRedirect
  // only falls through to Onboarding when URL is "/" + zero workers +
  // zero open sessions; once any state leaks in (deep-link, stale URL,
  // 401 with cached projection) that fall-through stops firing and the
  // user is stranded on the sidebar's no-machines empty state with no
  // way to reach Onboarding. /pair mounts Onboarding unconditionally
  // so the SidebarEmptyState CTA + Settings entry can always navigate
  // here for the cross-browser tap-to-pair / bootstrap-token flow.
  PAIR: "/pair",
  HELP: "/help",
  // /design — design-system phase 1 gallery. Renders every token + primitive
  // on one page as the visual reference. No auth, no params.
  DESIGN: "/design",
  FILE: "/file/:workerFp/*path",
  // /browse/:workerFp — Google-Drive-style file manager opened by the "+"
  // (new terminal) flow. Lists live dirs (filesListDir), drills into
  // subfolders, spawns a terminal in the picked folder. Path lives in local
  // state (not the URL) so "~" home works (decodeFolderPath mangles "~").
  // /browse redirects to the most-recent server.
  BROWSE: "/browse/:workerFp",
  BROWSE_ROOT: "/browse",
  SEARCH: "/search",
} as const;
