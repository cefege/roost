//! The browser's worker path codec: the ONE host implementation of
//! `roost_client_core::store::WorkerPaths`, plus the display and navigation
//! helpers (`short_worker_path`, crumbs, join, dirname) the sidebar, the deck,
//! the pane router and the browse picker share. Ports
//! `apps/web/src/lib/nativePath.ts` and `apps/web/src/lib/pathBasename.ts`;
//! every rule delegates to `roost-platform`'s codec, as v2 delegated to
//! `@roost/platform/native-path`.

pub mod palette;

use roost_client_core::store::WorkerPaths;
use roost_platform::{
    HostPlatform, decode_native_path_route, encode_native_path_route, native_path_basename,
    native_path_crumbs, native_path_dirname, native_path_identity_key, native_path_join,
};

/// The path codec every browser surface hands the client core.
///
/// A unit struct: the worker's platform arrives per call as its advertised `os`,
/// so one value serves every machine.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BrowserWorkerPaths;

impl WorkerPaths for BrowserWorkerPaths {
    fn folder_key(&self, worker_os: Option<&str>, path: &str) -> Option<String> {
        worker_path_identity(worker_os, path)
    }

    fn basename(&self, worker_os: Option<&str>, path: &str) -> Option<String> {
        worker_path_basename(worker_os, path)
    }
}

/// One breadcrumb: the label a trail shows and the canonical path it opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerPathCrumb {
    /// What the crumb reads as.
    pub label: String,
    /// The cumulative canonical path, so drive and UNC roots stay reversible.
    pub path: String,
}

/// The platform whose rules apply to `path` on a worker advertising `worker_os`.
///
/// A declared platform wins. Before the worker record hydrates (`None` or an
/// empty `os`), a Windows drive, UNC path or tagged route is unambiguous and
/// everything else reads as POSIX. An `os` this product does not support is
/// `None`: v2 refused it (`supportedWorkerPlatform` throws), and guessing
/// POSIX or Windows rules for it could merge two real folders.
pub fn worker_path_platform(worker_os: Option<&str>, path: &str) -> Option<HostPlatform> {
    match worker_os {
        None | Some("") => Some(infer_native_path_platform(path)),
        Some(os) => HostPlatform::parse(os).ok(),
    }
}

/// v2 `inferNativePathPlatform`: Windows only when the spelling is Windows-only.
pub fn infer_native_path_platform(path_or_route: &str) -> HostPlatform {
    if is_windows_route(path_or_route)
        || is_windows_drive_absolute(path_or_route)
        || is_windows_unc(path_or_route)
    {
        HostPlatform::Windows
    } else {
        HostPlatform::Linux
    }
}

/// The identity key two spellings of one directory share, or `None` when the
/// codec refuses the path.
pub fn worker_path_identity(worker_os: Option<&str>, path: &str) -> Option<String> {
    let platform = worker_path_platform(worker_os, path)?;
    native_path_identity_key(platform, &path_for_codec(platform, path)).ok()
}

/// The last segment of a worker path.
pub fn worker_path_basename(worker_os: Option<&str>, path: &str) -> Option<String> {
    let platform = worker_path_platform(worker_os, path)?;
    native_path_basename(platform, &path_for_codec(platform, path)).ok()
}

/// The directory containing a worker path; a root is its own parent.
pub fn worker_path_dirname(worker_os: Option<&str>, path: &str) -> Option<String> {
    let platform = worker_path_platform(worker_os, path)?;
    native_path_dirname(platform, &path_for_codec(platform, path)).ok()
}

/// Relative `parts` joined under `base`; `None` when a part is absolute.
pub fn join_worker_path(worker_os: Option<&str>, base: &str, parts: &[&str]) -> Option<String> {
    let platform = worker_path_platform(worker_os, base)?;
    let converted: Vec<String> = parts
        .iter()
        .map(|part| path_for_codec(platform, part))
        .collect();
    let borrowed: Vec<&str> = converted.iter().map(String::as_str).collect();
    native_path_join(platform, &path_for_codec(platform, base), &borrowed).ok()
}

/// The breadcrumb trail, root first; empty when the codec refuses the path.
pub fn worker_path_crumbs(worker_os: Option<&str>, path: &str) -> Vec<WorkerPathCrumb> {
    let Some(platform) = worker_path_platform(worker_os, path) else {
        return Vec::new();
    };
    native_path_crumbs(platform, &path_for_codec(platform, path))
        .map(|crumbs| {
            crumbs
                .into_iter()
                .map(|crumb| WorkerPathCrumb {
                    label: crumb.name,
                    path: crumb.path,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Whether two paths name one directory on one worker.
pub fn same_worker_path(worker_os: Option<&str>, left: &str, right: &str) -> bool {
    let probe = if left.is_empty() { right } else { left };
    let Some(platform) = worker_path_platform(worker_os, probe) else {
        return left == right;
    };
    match (
        native_path_identity_key(platform, &path_for_codec(platform, left)),
        native_path_identity_key(platform, &path_for_codec(platform, right)),
    ) {
        (Ok(left_key), Ok(right_key)) => left_key == right_key,
        _ => left == right,
    }
}

/// The route segment a worker path is linked as (v2 `encodeWorkerPathRoute`).
pub fn encode_worker_path_route(worker_os: Option<&str>, path: &str) -> Option<String> {
    let platform = worker_path_platform(worker_os, path)?;
    encode_native_path_route(platform, &path_for_codec(platform, path)).ok()
}

/// The worker path a route segment names (v2 `decodeWorkerPathRoute`). A tagged
/// Windows route decodes before the worker record hydrates.
pub fn decode_worker_path_route(worker_os: Option<&str>, route: &str) -> Option<String> {
    let platform = worker_path_platform(worker_os, route)?;
    decode_native_path_route(platform, route).ok()
}

/// The compact label tabs and the sidebar show (v2 `shortWorkerPath`): the
/// historical `user/basename` form under a `Users` or `home` root, otherwise the
/// native basename. A path the codec refuses reads as itself.
pub fn short_worker_path(worker_os: Option<&str>, path: &str) -> String {
    let crumbs = worker_path_crumbs(worker_os, path);
    let Some(last) = crumbs.last().map(|crumb| crumb.label.as_str()) else {
        return path.to_owned();
    };
    let user_root = crumbs.iter().position(|crumb| {
        let name = crumb.label.to_lowercase();
        name == "users" || name == "home"
    });
    let user = user_root
        .and_then(|index| crumbs.get(index + 1))
        .map(|crumb| crumb.label.as_str());
    match user {
        Some(user) if user != last => format!("{user}/{last}"),
        _ => last.to_owned(),
    }
}

/// Windows backslashes are accepted at this boundary and handed to the codec in
/// its canonical forward-slash form.
fn path_for_codec(platform: HostPlatform, path: &str) -> String {
    if platform == HostPlatform::Windows {
        path.replace('\\', "/")
    } else {
        path.to_owned()
    }
}

/// `^~(?:drive|unc)(?:/|$)`.
fn is_windows_route(path: &str) -> bool {
    ["~drive", "~unc"].iter().any(|tag| {
        path.strip_prefix(tag)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

/// `^[A-Za-z]:[\\/]`.
fn is_windows_drive_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

/// `^(?:\\\\|//)[^\\/]+[\\/][^\\/]+`: a server AND a share after the prefix.
fn is_windows_unc(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("\\\\").or_else(|| path.strip_prefix("//")) else {
        return false;
    };
    let mut fields = rest.splitn(2, ['/', '\\']);
    let server = fields.next().unwrap_or_default();
    let share = fields
        .next()
        .and_then(|tail| tail.split(['/', '\\']).next())
        .unwrap_or_default();
    !server.is_empty() && !share.is_empty()
}
