//! Stable terminal URLs: `/t/:workerFp/*folderPath` keys a terminal on (machine,
//! spawn folder) rather than the ephemeral session id, so a bookmark survives a
//! session dying and respawning in the same folder. Ports
//! `apps/web/src/lib/terminalHref.ts`; read by the sidebar rows, the deck tabs
//! and `route_session` (decoding). The path rules are `roost-platform`'s route
//! codec, applied through `platform::worker_paths::worker_path_platform`.

use roost_client_core::Store;
use roost_client_core::store::Session;
use roost_platform::{decode_native_path_route, encode_native_path_route};

use crate::platform::worker_paths::worker_path_platform;
use crate::routes::{Route, session_href};

/// An absolute folder as a route splat: POSIX percent-encoded with the leading
/// slash dropped, Windows roots tagged. A path the codec refuses (or a worker OS
/// this product does not support) is returned as written, which keeps the link
/// the reader clicked rather than inventing another.
pub fn encode_folder_path(worker_os: Option<&str>, abs: &str) -> String {
    let Some(platform) = worker_path_platform(worker_os, abs) else {
        return abs.to_owned();
    };
    let canonical = if platform == roost_platform::HostPlatform::Windows {
        abs.replace('\\', "/")
    } else {
        abs.to_owned()
    };
    encode_native_path_route(platform, &canonical).unwrap_or_else(|_| abs.to_owned())
}

/// A route splat back to the worker path. `None` when the splat is not a route
/// the codec accepts, which the caller treats as "no session here".
pub fn decode_folder_path(worker_os: Option<&str>, splat: &str) -> Option<String> {
    let platform = worker_path_platform(worker_os, splat)?;
    decode_native_path_route(platform, splat).ok()
}

/// The stable URL for a session: `/t/<fp>/<folder>` from its spawn folder, or
/// `/s/<id>` for an older row with no spawn folder.
pub fn terminal_href(store: &Store, session: &Session) -> String {
    let Some(folder) = session.spawn_cwd.as_deref().filter(|folder| !folder.is_empty()) else {
        return session_href(session.id.as_str());
    };
    let worker_os = worker_os(store, session.worker_fp.as_str());
    Route::Terminal {
        worker_fp: session.worker_fp.as_str().to_owned(),
        folder_path: encode_folder_path(worker_os, folder),
    }
    .to_path()
}

/// The worker's advertised OS, when its record has hydrated.
pub fn worker_os<'store>(store: &'store Store, worker_fp: &str) -> Option<&'store str> {
    store
        .workers
        .get(worker_fp)
        .map(|worker| worker.os.as_str())
        .filter(|os| !os.is_empty())
}
