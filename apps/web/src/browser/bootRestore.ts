// Boot-restore one-shot guard (extracted from App.tsx so MainPane can share it).
//
// Only the FIRST render of "/" per page load bounces into the last-visited
// terminal (App.tsx WorkspaceRedirect); after that we fall through to
// HomeLanding — no redirect loop. MainPane's dead-terminal escape ("Go home")
// also consumes the guard so an explicit exit lands on Home instead of
// ping-ponging straight back into the last (possibly dead) terminal via the
// boot-restore.

let consumed = false;

/** True only on the first "/" render of this page load; false forever after. */
export function shouldBootRestore(): boolean {
  return !consumed;
}

/** Spend the one-shot boot-restore (WorkspaceRedirect's first render, or an
 *  explicit "Go home"). Idempotent. */
export function consumeBootRestore(): void {
  consumed = true;
}
