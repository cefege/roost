//! Path identity: the differences the host filesystem itself treats as one
//! path, and nothing else. Folding more than this merges two real
//! directories, which is a merge a user cannot undo.

use crate::host_platform::HostPlatform;
use crate::native_path::normalize::{NativePathError, normalize_native_path};

/// The three macOS system symlinks: `/tmp`, `/var` and `/etc` ARE
/// `/private/{tmp,var,etc}`. Folding only these three is exact, not a
/// heuristic — any other `/private/x` is a real directory and must stay
/// distinct. The pair matters because one directory otherwise gets two keys
/// depending on whether the value came from what a user typed or from a
/// realpath'd source such as a session's `cwd`, which the shell reports over
/// OSC 7.
pub const DARWIN_PRIVATE_ROOTS: [&str; 3] = ["tmp", "var", "etc"];

/// The key two paths share when they name one directory. Display is never
/// folded here — that is [`super::normalize_native_path`]'s job.
pub fn native_path_identity_key(
    platform: HostPlatform,
    path: &str,
) -> Result<String, NativePathError> {
    let normalized = normalize_native_path(platform, path)?;
    Ok(match platform {
        HostPlatform::Linux => normalized,
        HostPlatform::MacOs => fold_darwin_system_symlinks(&normalized),
        HostPlatform::Windows => normalized.to_lowercase(),
    })
}

fn fold_darwin_system_symlinks(normalized: &str) -> String {
    let Some(after_root_slash) = normalized.strip_prefix('/') else {
        return normalized.to_owned();
    };
    for root in DARWIN_PRIVATE_ROOTS {
        let is_the_root_itself = after_root_slash == root;
        let is_under_the_root = after_root_slash
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'));
        if is_the_root_itself || is_under_the_root {
            return format!("/private{normalized}");
        }
    }
    normalized.to_owned()
}

/// Same-directory test for a worker-advertised `os` string, for callers that
/// hold a raw `os` column rather than a validated platform. An unknown `os`,
/// or a path either side cannot normalize (relative, drive-relative), falls
/// back to exact equality — never a false merge.
pub fn same_worker_folder(os: &str, left: &str, right: &str) -> bool {
    let Ok(platform) = HostPlatform::parse(os) else {
        return left == right;
    };
    match (
        native_path_identity_key(platform, left),
        native_path_identity_key(platform, right),
    ) {
        (Ok(left_key), Ok(right_key)) => left_key == right_key,
        _ => left == right,
    }
}
