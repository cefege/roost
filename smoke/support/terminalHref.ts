// Stable terminal URL helpers. The /t/:workerFp/*folderPath route keys a
// terminal on (server, spawn folder) instead of the ephemeral session id, so a
// bookmark survives a session death+respawn in the same folder.
//
// The page's own codec is `crates/roost-web/src/terminal_href.rs`; specs build
// the URL they expect the page to navigate to with this POSIX form, which is
// the only worker platform the oracle runs.

// Absolute folder path → splat segment: each segment URI-encoded, the leading
// slash dropped.
export function encodeFolderPath(abs: string): string {
  if (!abs.startsWith("/")) throw new Error(`POSIX worker path must be absolute: ${abs}`);
  return abs
    .split("/")
    .map((segment) => (segment ? encodeURIComponent(segment) : segment))
    .join("/")
    .replace(/^\//, "");
}
