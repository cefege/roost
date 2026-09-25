//! Canonical lexical worker paths. Existing-path realpath resolution happens
//! at the worker boundary; nothing here touches a filesystem, so a coordinator
//! can fold a path it has never seen. `std::path` is deliberately not used: it
//! follows the rules of the host this process runs on, and the point of this
//! module is to implement both sets of rules explicitly.

use thiserror::Error;

use crate::host_platform::HostPlatform;

/// Every way a native path can be refused. One variant per condition, so a
/// caller can match the cause rather than parse a message.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NativePathError {
    #[error("native path must be non-empty and contain no NUL bytes")]
    EmptyOrNul,
    #[error("native path escapes its root")]
    EscapesRoot,
    #[error("POSIX worker path must be absolute: {0}")]
    PosixNotAbsolute(String),
    #[error("drive-relative Windows path is not allowed: {0}")]
    DriveRelative(String),
    #[error("UNC path requires server and share: {0}")]
    UncMissingServerShare(String),
    #[error("Windows worker path must be drive-absolute or UNC: {0}")]
    WindowsNotDriveAbsoluteOrUnc(String),
    #[error("the browse home sentinel must be resolved before a filesystem operation")]
    HomeSentinelUnresolvedForFilesystem,
    #[error("the browse home sentinel must be resolved before route encoding")]
    HomeSentinelUnresolvedForRoute,
    #[error("nativePathJoin only accepts relative child parts: {0}")]
    JoinPartNotRelative(String),
    #[error("invalid encoded native path segment")]
    InvalidRouteSegment,
    #[error("native path route has malformed percent-encoding")]
    MalformedRouteEncoding,
    #[error("invalid Windows drive route")]
    InvalidDriveRoute,
    #[error("invalid Windows UNC route")]
    InvalidUncRoute,
    #[error("Windows native path route is missing a tagged root")]
    MissingTaggedRoot,
}

/// Canonical lexical worker path: absolute, no `.`/`..`, no repeated
/// separator, `\` folded to `/`, and the `~` sentinel preserved. Folding a
/// display form is this function's job; folding what the host filesystem
/// treats as one path is [`super::native_path_identity_key`]'s.
pub fn normalize_native_path(
    platform: HostPlatform,
    input: &str,
) -> Result<String, NativePathError> {
    reject_invalid_path(input)?;
    if let Some(sentinel) = normalize_home_sentinel(input)? {
        return Ok(sentinel);
    }
    match platform {
        HostPlatform::MacOs | HostPlatform::Linux => normalize_posix(input),
        HostPlatform::Windows => normalize_windows(input),
    }
}

pub(crate) fn reject_invalid_path(path: &str) -> Result<(), NativePathError> {
    if path.is_empty() || path.contains('\0') {
        return Err(NativePathError::EmptyOrNul);
    }
    Ok(())
}

/// `~` and `~/…` are the browse home sentinel, on every platform, and nothing
/// else starting with a tilde is. The sentinel survives normalization because
/// the browse surface only resolves it when it reaches a filesystem.
fn normalize_home_sentinel(input: &str) -> Result<Option<String>, NativePathError> {
    let path = input.replace('\\', "/");
    if path != "~" && !path.starts_with("~/") {
        return Ok(None);
    }
    let segments = normalize_segments(&path, 1)?;
    if segments.len() == 1 {
        return Ok(Some("~".to_owned()));
    }
    Ok(Some(format!("~/{}", segments[1..].join("/"))))
}

/// Resolve `.` and `..` over an already-split path. The first `floor`
/// segments are the root and pass through untouched — a `..` in a UNC server
/// or share position is a real name there, not a pop. A `..` with nothing
/// left to pop is refused rather than clamped to the root, because a clamped
/// path is a path the caller did not ask for.
pub(crate) fn normalize_segments(path: &str, floor: usize) -> Result<Vec<String>, NativePathError> {
    let mut out: Vec<String> = path.split('/').take(floor).map(str::to_owned).collect();
    for segment in path.split('/').skip(floor) {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            if out.len() == floor {
                return Err(NativePathError::EscapesRoot);
            }
            out.pop();
            continue;
        }
        out.push(segment.to_owned());
    }
    Ok(out)
}

fn normalize_posix(input: &str) -> Result<String, NativePathError> {
    if !input.starts_with('/') {
        return Err(NativePathError::PosixNotAbsolute(input.to_owned()));
    }
    let segments = normalize_segments(input, 1)?;
    if segments.len() == 1 {
        return Ok("/".to_owned());
    }
    Ok(format!("/{}", segments[1..].join("/")))
}

fn normalize_windows(input: &str) -> Result<String, NativePathError> {
    let path = input.replace('\\', "/");
    if let Some((drive_letter, tail)) = split_drive_anchor(&path) {
        let tail = normalize_segments(tail, 0)?;
        if tail.is_empty() {
            return Ok(format!("{drive_letter}:/"));
        }
        return Ok(format!("{drive_letter}:/{}", tail.join("/")));
    }
    // `C:foo` is drive-relative: it resolves against the process's own
    // per-drive working directory, which a remote worker has no meaning for.
    if starts_with_drive_letter(&path) {
        return Err(NativePathError::DriveRelative(input.to_owned()));
    }
    if let Some(parts) = path.strip_prefix("//") {
        let mut fields = parts.splitn(3, '/');
        let (server, share) = (fields.next(), fields.next());
        match (server, share) {
            (Some(server), Some(share)) if !server.is_empty() && !share.is_empty() => {
                let normalized = normalize_segments(parts, 2)?;
                return Ok(format!("//{}", normalized.join("/")));
            }
            _ => return Err(NativePathError::UncMissingServerShare(input.to_owned())),
        }
    }
    Err(NativePathError::WindowsNotDriveAbsoluteOrUnc(
        input.to_owned(),
    ))
}

/// `^([A-Za-z]):(?:\/(.*))?$` — a drive and either nothing after the colon or a
/// rooted tail. `C:foo` deliberately does not match, so it can be refused as
/// drive-relative rather than treated as a drive.
pub(crate) fn split_drive_anchor(path: &str) -> Option<(String, &str)> {
    let bytes = path.as_bytes();
    if bytes.len() < 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return None;
    }
    let drive_letter = (bytes[0] as char).to_ascii_uppercase().to_string();
    match path.get(2..) {
        // `C:` and `C:/` are both the drive root. The regex this ports,
        // `^([A-Za-z]):(?:\/(.*))?$`, makes the whole tail optional, so a
        // bare drive normalizes to `C:/` and is not drive-relative.
        None | Some("") => Some((drive_letter, "")),
        Some(tail) if tail.starts_with('/') => Some((drive_letter, &tail[1..])),
        Some(_) => None,
    }
}

/// `^[A-Za-z]:` — a drive letter with anything (or nothing) after it.
pub(crate) fn starts_with_drive_letter(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

#[cfg(test)]
mod tests {
    use super::{
        NativePathError, normalize_native_path, split_drive_anchor, starts_with_drive_letter,
    };
    use crate::host_platform::HostPlatform;

    #[test]
    fn a_posix_path_folds_separators_and_dot_segments() {
        assert_eq!(
            normalize_native_path(HostPlatform::Linux, "/a//b/./c/").as_deref(),
            Ok("/a/b/c")
        );
        assert_eq!(
            normalize_native_path(HostPlatform::MacOs, "/a/b/../c").as_deref(),
            Ok("/a/c")
        );
        assert_eq!(
            normalize_native_path(HostPlatform::Linux, "/").as_deref(),
            Ok("/")
        );
    }

    #[test]
    fn a_backslash_is_an_ordinary_character_in_a_posix_segment() {
        assert_eq!(
            normalize_native_path(HostPlatform::Linux, r"/a\b").as_deref(),
            Ok(r"/a\b")
        );
    }

    #[test]
    fn a_dot_dot_that_pops_past_the_root_is_refused_not_clamped() {
        assert_eq!(
            normalize_native_path(HostPlatform::Linux, "/.."),
            Err(NativePathError::EscapesRoot)
        );
        assert_eq!(
            normalize_native_path(HostPlatform::Linux, "/a/../.."),
            Err(NativePathError::EscapesRoot)
        );
        assert_eq!(
            normalize_native_path(HostPlatform::Linux, "/a/..").as_deref(),
            Ok("/")
        );
    }

    #[test]
    fn a_relative_posix_path_is_refused() {
        assert_eq!(
            normalize_native_path(HostPlatform::Linux, "a/b"),
            Err(NativePathError::PosixNotAbsolute("a/b".to_owned()))
        );
    }

    #[test]
    fn only_a_tilde_and_a_tilde_slash_are_the_home_sentinel() {
        for platform in HostPlatform::ALL {
            assert_eq!(normalize_native_path(platform, "~").as_deref(), Ok("~"));
            assert_eq!(
                normalize_native_path(platform, "~/proj/./src").as_deref(),
                Ok("~/proj/src")
            );
            assert_eq!(
                normalize_native_path(platform, r"~\proj").as_deref(),
                Ok("~/proj")
            );
            // Which refusal a `~user/…` path gets is the platform's own
            // answer: POSIX says "not absolute", Windows says "not a drive or
            // a UNC share". Same input, two contracts.
            let refusal = match platform {
                HostPlatform::Windows => {
                    NativePathError::WindowsNotDriveAbsoluteOrUnc("~user/proj".to_owned())
                }
                _ => NativePathError::PosixNotAbsolute("~user/proj".to_owned()),
            };
            assert_eq!(normalize_native_path(platform, "~user/proj"), Err(refusal));
        }
        assert_eq!(
            normalize_native_path(HostPlatform::Linux, "~/.."),
            Err(NativePathError::EscapesRoot)
        );
    }

    #[test]
    fn a_windows_drive_is_uppercased_and_rooted() {
        for (input, expected) in [
            ("c:", "C:/"),
            ("C:/", "C:/"),
            (r"c:\proj\src", "C:/proj/src"),
            ("C:/proj/./src/../lib", "C:/proj/lib"),
            ("C:/proj/..", "C:/"),
        ] {
            assert_eq!(
                normalize_native_path(HostPlatform::Windows, input).as_deref(),
                Ok(expected),
                "input was {input}"
            );
        }
    }

    #[test]
    fn a_drive_relative_windows_path_is_refused() {
        assert_eq!(
            normalize_native_path(HostPlatform::Windows, "C:proj"),
            Err(NativePathError::DriveRelative("C:proj".to_owned()))
        );
    }

    #[test]
    fn a_unc_path_needs_a_server_and_a_share() {
        assert_eq!(
            normalize_native_path(HostPlatform::Windows, r"\\host\share\proj").as_deref(),
            Ok("//host/share/proj")
        );
        assert_eq!(
            normalize_native_path(HostPlatform::Windows, "//host"),
            Err(NativePathError::UncMissingServerShare("//host".to_owned()))
        );
        assert_eq!(
            normalize_native_path(HostPlatform::Windows, "//host/"),
            Err(NativePathError::UncMissingServerShare("//host/".to_owned()))
        );
    }

    #[test]
    fn a_windows_path_that_is_neither_drive_nor_unc_is_refused() {
        assert_eq!(
            normalize_native_path(HostPlatform::Windows, "/proj"),
            Err(NativePathError::WindowsNotDriveAbsoluteOrUnc(
                "/proj".to_owned()
            ))
        );
    }

    #[test]
    fn a_unc_root_segment_is_a_name_not_something_to_pop() {
        assert_eq!(
            normalize_native_path(HostPlatform::Windows, "//host/share/../.."),
            Err(NativePathError::EscapesRoot)
        );
        // The root segments themselves are names, not something to pop.
        assert_eq!(
            normalize_native_path(HostPlatform::Windows, "//./x/./y").as_deref(),
            Ok("//./x/y")
        );
    }

    #[test]
    fn an_empty_or_nul_bearing_path_is_refused_before_anything_else() {
        for platform in HostPlatform::ALL {
            assert_eq!(
                normalize_native_path(platform, ""),
                Err(NativePathError::EmptyOrNul)
            );
            assert_eq!(
                normalize_native_path(platform, "/a\0b"),
                Err(NativePathError::EmptyOrNul)
            );
        }
    }

    #[test]
    fn the_drive_shapes_are_read_the_way_the_browser_route_speaks_them() {
        assert_eq!(split_drive_anchor("C:/a"), Some(("C".to_owned(), "a")));
        assert_eq!(split_drive_anchor("c:"), Some(("C".to_owned(), "")));
        assert_eq!(split_drive_anchor("C:a"), None);
        assert_eq!(split_drive_anchor("1:/a"), None);
        assert!(starts_with_drive_letter("C:a"));
        assert!(starts_with_drive_letter("C:"));
        assert!(!starts_with_drive_letter("/a"));
    }
}
