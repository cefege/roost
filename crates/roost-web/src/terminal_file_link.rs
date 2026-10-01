//! The worker-aware file resolver a terminal link attachment scans with: a
//! path a process printed, resolved against the folder its terminal is in, into
//! the `/file/…` route the file viewer opens. Pure and target-independent, so
//! the pane binds its live session to it at mount
//! (`components::terminal::pane_mount::link_targets`) and the rules test
//! natively. Ports `resolveFile` of
//! `apps/web/src/components/terminal/cell-terminal-input.ts`.
//!
//! WITHOUT THIS RESOLVER NO FILE LINK EXISTS. The detector classifies a path
//! only when a resolver will mint a route for it, because a `file:` anchor with
//! no `href` opens nothing — so a pane that omits the resolver paints terminal
//! paths as the plain text they arrived as.

use roost_platform::HostPlatform;

use crate::platform::worker_paths::{resolve_worker_path, worker_path_platform};
use crate::terminal_href::worker_file_href;

/// One terminal-printed target resolved against one session. `file_authority`
/// is the `file://host` of a `file://` URI, present for no other spelling.
pub fn resolve_terminal_file(
    worker_os: Option<&str>,
    worker_fp: &str,
    cwd: &str,
    raw_path: &str,
    line: Option<u64>,
    file_authority: Option<&str>,
) -> Option<String> {
    let platform = worker_path_platform(worker_os, cwd)?;
    // `//host/share` is a protocol-relative URL until a Windows machine says
    // otherwise, so only Windows is allowed to read it as the path it looks
    // like.
    if raw_path.starts_with("//") && platform != HostPlatform::Windows {
        return None;
    }
    let local_path = match file_authority.filter(|_| platform == HostPlatform::Windows) {
        Some(authority) => format!(
            "//{authority}{}",
            raw_path
                .strip_prefix('/')
                .map_or_else(|| format!("/{raw_path}"), |rooted| rooted.to_owned())
        ),
        None => raw_path.to_owned(),
    };
    let absolute = resolve_worker_path(worker_os, cwd, &local_path)?;
    worker_file_href(worker_os, worker_fp, &absolute, line)
}
