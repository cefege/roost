// Route definitions. URL is source of truth for nav state (R0.18).
// Consumed by App.tsx's <Router> via @solidjs/router RouteDefinition.

export const ROUTES = {
  ROOT: "/",
  // Managed account entry and recovery stay public until they bind or replace
  // this coordinator-origin browser identity.
  LOGIN: "/login",
  SIGNUP: "/signup",
  SIGNUP_VERIFY: "/signup/verify",
  GOOGLE_COMPLETE: "/auth/google/complete",
  ACTIVATE: "/activate",
  FORGOT_PASSWORD: "/forgot-password",
  RESET_PASSWORD: "/reset-password",
  // Authenticated managed entry. Existing terminal deep links remain unchanged
  // and are guarded globally once the public coordinator identity says SaaS.
  APP: "/app",
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
  // Removed status-specific routes; all sessions render in the single list.
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

// Concrete-href builders — the only sanctioned way to navigate to a
// parameterized route. Raw template strings drift silently from these
// patterns when a route shape changes (compiles clean, breaks deep links,
// ⌘-switcher targets and boot restore at once).
export function sessionHref(sessionId: string): string {
  return `/s/${sessionId}`;
}
export function browseHref(workerFp: string): string {
  return `/browse/${workerFp}`;
}
export function settingsPaneHref(pane: string): string {
  return `/settings/${pane}`;
}
