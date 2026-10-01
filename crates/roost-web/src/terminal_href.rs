//! Stable URLs a worker path is written as: `/t/:workerFp/*folderPath` keys a
//! terminal on (machine, spawn folder) rather than the ephemeral session id, so
//! a bookmark survives a session dying and respawning in the same folder, and
//! `/file/:workerFp/*path` names one file on one machine. Ports
//! `apps/web/src/lib/terminalHref.ts` and `workerFileHref` /
//! `parseWorkerFileHref` of `apps/web/src/lib/nativePath.ts`.
//!
//! Minting and reading a file route are here together, and the file viewer is
//! the only reader, because a route carries a splat: a producer that encodes
//! and a consumer that does not decode disagree about every file, and only one
//! of them can be right about the grammar. The path rules are `roost-platform`'s
//! route codec, applied through `platform::worker_paths`.

use roost_client_core::Store;
use roost_client_core::store::Session;
use roost_platform::encode_native_path_route;

use crate::platform::worker_paths::palette::child_path;
use crate::platform::worker_paths::{
    decode_worker_path_route, encode_worker_path_route, worker_path_platform,
};
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

/// `/file/<workerFp>/<route>` for one absolute path on one machine, with the
/// 1-based line a process printed after it. `None` when the route codec
/// refuses the path, which is what keeps a link off a target that is not a
/// file: the detector drops a segment whose route will not mint, and a browse
/// row that cannot mint one is a row with no link.
///
/// The one builder for this route. The terminal linkifier and the browse tree
/// both go through it, so a path a process printed and a file a directory
/// lists are the same URL.
#[must_use]
pub fn worker_file_href(
    worker_os: Option<&str>,
    worker_fp: &str,
    path: &str,
    line: Option<u64>,
) -> Option<String> {
    let href = Route::File {
        worker_fp: worker_fp.to_owned(),
        path: encode_worker_path_route(worker_os, path)?,
    }
    .to_path();
    Some(match line.filter(|line| *line > 0) {
        Some(line) => format!("{href}#L{line}"),
        None => href,
    })
}

/// The `/file/…` href for `name` as it is listed inside `dir`: a browse row's
/// link, minted exactly the way a printed path's is.
#[must_use]
pub fn child_file_href(
    worker_os: Option<&str>,
    worker_fp: &str,
    dir: &str,
    name: &str,
) -> Option<String> {
    let absolute = child_path(worker_os, dir, name)?;
    worker_file_href(worker_os, worker_fp, &absolute, None)
}

/// The machine and the absolute path a file route names, or `None` when it
/// names no file this product can open.
///
/// The inverse of [`worker_file_href`], and the half the file viewer used to be
/// missing: the route carries the path as the grammar's splat, so turning it
/// back into something a worker will read is the platform codec's job. A splat
/// it refuses is a file the sheet shows nothing for, rather than a path sent to
/// the worker and refused there.
#[must_use]
pub fn file_target(worker_os: Option<&str>, route: &Route) -> Option<(String, String)> {
    let Route::File { worker_fp, path } = route else {
        return None;
    };
    let path = decode_worker_path_route(worker_os, path)?;
    Some((worker_fp.clone(), path))
}

/// A route splat back to the worker path. `None` when the splat is not a route
/// the codec accepts, which the caller treats as "no session here".
pub fn decode_folder_path(worker_os: Option<&str>, splat: &str) -> Option<String> {
    decode_worker_path_route(worker_os, splat)
}

/// The stable URL for a session: `/t/<fp>/<folder>` from its spawn folder, or
/// `/s/<id>` for an older row with no spawn folder.
pub fn terminal_href(store: &Store, session: &Session) -> String {
    let Some(folder) = session
        .spawn_cwd
        .as_deref()
        .filter(|folder| !folder.is_empty())
    else {
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
