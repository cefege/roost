//! The parts of a path: its last segment, everything before it, a relative
//! join under it, and the breadcrumb trail a browse surface walks. All four
//! normalize first, so a caller never has to ask whether its input was already
//! canonical — except basename, which is the one place a relative path still
//! has a sensible answer.

use crate::host_platform::HostPlatform;
use crate::native_path::normalize::{
    NativePathError, normalize_native_path, reject_invalid_path, starts_with_drive_letter,
};

/// One step of a browse trail: the label to show and the path it navigates to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePathCrumb {
    pub name: String,
    pub path: String,
}

/// The last segment. A path that is not absolute keeps its own last segment
/// rather than being refused: a caller holding an already-relative name still
/// wants the name, and a wrong answer here would be a wrong breadcrumb.
pub fn native_path_basename(platform: HostPlatform, path: &str) -> Result<String, NativePathError> {
    reject_invalid_path(path)?;
    let is_absolute = path.starts_with('/')
        || path == "~"
        || path.starts_with("~/")
        || (platform == HostPlatform::Windows
            && (is_drive_absolute(path) || path.starts_with("\\\\")));
    if !is_absolute {
        let token = path.replace('\\', "/");
        let token = token.trim_end_matches('/');
        return Ok(last_segment(token).to_owned());
    }
    let normalized = normalize_native_path(platform, path)?;
    if normalized == "~" {
        return Ok("~".to_owned());
    }
    if normalized == "/" {
        return Ok("/".to_owned());
    }
    if platform == HostPlatform::Windows && is_windows_drive_root(&normalized) {
        return Ok(normalized[..2].to_owned());
    }
    Ok(last_segment(&normalized).to_owned())
}

/// Everything before the last segment. Roots are their own parent, so a path
/// never walks off the top.
pub fn native_path_dirname(platform: HostPlatform, path: &str) -> Result<String, NativePathError> {
    let normalized = normalize_native_path(platform, path)?;
    if normalized == "~" {
        return Ok("~".to_owned());
    }
    if normalized.starts_with("~/") && normalized.rfind('/') == Some(1) {
        return Ok("~".to_owned());
    }
    if normalized == "/"
        || (platform == HostPlatform::Windows && is_windows_drive_root(&normalized))
    {
        return Ok(normalized);
    }
    if platform == HostPlatform::Windows && normalized.starts_with("//") {
        let mut fields: Vec<&str> = normalized[2..].split('/').collect();
        if fields.len() <= 2 {
            return Ok(normalized);
        }
        fields.pop();
        return Ok(format!("//{}", fields.join("/")));
    }
    let last_separator = normalized.rfind('/');
    if platform == HostPlatform::Windows && last_separator == Some(2) {
        return Ok(normalized[..3].to_owned());
    }
    match last_separator {
        Some(index) if index > 0 => Ok(normalized[..index].to_owned()),
        _ => Ok("/".to_owned()),
    }
}

/// Join relative child parts under a base. A part that is itself absolute, or
/// that names a drive, is refused: silently letting it win would turn a join
/// into a jump, and the caller would have no way to see that happen.
pub fn native_path_join(
    platform: HostPlatform,
    base: &str,
    parts: &[&str],
) -> Result<String, NativePathError> {
    let mut joined = normalize_native_path(platform, base)?;
    for part in parts {
        reject_invalid_path(part)?;
        let clean = part.replace('\\', "/");
        if clean.starts_with('/') || starts_with_drive_letter(&clean) {
            return Err(NativePathError::JoinPartNotRelative((*part).to_owned()));
        }
        let trimmed = joined.strip_suffix('/').unwrap_or(&joined);
        joined = format!("{trimmed}/{clean}");
    }
    normalize_native_path(platform, &joined)
}

/// The breadcrumb trail, root first.
pub fn native_path_crumbs(
    platform: HostPlatform,
    path: &str,
) -> Result<Vec<NativePathCrumb>, NativePathError> {
    let normalized = normalize_native_path(platform, path)?;
    if normalized == "~" || normalized.starts_with("~/") {
        let mut crumbs = vec![crumb("~", "~")];
        let mut current = "~".to_owned();
        // The bare sentinel has no `~/` to slice past, so its trail stops at
        // the sentinel rather than indexing off the end of a one-byte string.
        let Some(rest) = normalized.strip_prefix("~/") else {
            return Ok(crumbs);
        };
        for segment in segments(rest) {
            current = format!("{current}/{segment}");
            crumbs.push(crumb(segment, &current));
        }
        return Ok(crumbs);
    }
    if platform == HostPlatform::Windows {
        if let Some((drive_letter, tail)) = split_windows_drive_root(&normalized) {
            let root = format!("{drive_letter}:/");
            let mut crumbs = vec![crumb(&root, &root)];
            let mut current = root;
            for segment in segments(tail) {
                let trimmed = current.strip_suffix('/').unwrap_or(&current);
                current = format!("{trimmed}/{segment}");
                crumbs.push(crumb(segment, &current));
            }
            return Ok(crumbs);
        }
        let fields: Vec<&str> = normalized[2..].split('/').collect();
        let root = format!("//{}/{}", fields[0], fields[1]);
        let mut crumbs = vec![crumb(&root, &root)];
        let mut current = root;
        for segment in &fields[2..] {
            current = format!("{current}/{segment}");
            crumbs.push(crumb(segment, &current));
        }
        return Ok(crumbs);
    }
    let mut crumbs = vec![crumb("/", "/")];
    let mut current = String::new();
    for segment in segments(&normalized) {
        current = format!("{current}/{segment}");
        crumbs.push(crumb(segment, &current));
    }
    Ok(crumbs)
}

fn crumb(name: &str, path: &str) -> NativePathCrumb {
    NativePathCrumb {
        name: name.to_owned(),
        path: path.to_owned(),
    }
}

fn segments(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|segment| !segment.is_empty())
}

fn last_segment(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `^[A-Za-z]:[\\/]` on the raw input, before any folding.
fn is_drive_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'/' || bytes[2] == b'\\')
}

/// `^([A-Z]):\/(.*)$` on an already-normalized Windows path. The tail may be
/// empty, so `C:/` is a drive root and not a bare drive letter.
fn split_windows_drive_root(normalized: &str) -> Option<(&str, &str)> {
    let bytes = normalized.as_bytes();
    let is_drive_root =
        bytes.len() >= 3 && bytes[0].is_ascii_uppercase() && bytes[1] == b':' && bytes[2] == b'/';
    if is_drive_root {
        Some((&normalized[..1], &normalized[3..]))
    } else {
        None
    }
}

/// `^[A-Z]:\/$` on an already-normalized Windows path.
fn is_windows_drive_root(normalized: &str) -> bool {
    split_windows_drive_root(normalized).is_some_and(|(_, tail)| tail.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{
        NativePathCrumb, native_path_basename, native_path_crumbs, native_path_dirname,
        native_path_join,
    };
    use crate::host_platform::HostPlatform;
    use crate::native_path::normalize::NativePathError;

    fn trail(crumbs: &[NativePathCrumb]) -> Vec<(&str, &str)> {
        crumbs
            .iter()
            .map(|crumb| (crumb.name.as_str(), crumb.path.as_str()))
            .collect()
    }

    #[test]
    fn a_posix_trail_starts_at_the_root_and_ends_at_the_path() {
        let crumbs = native_path_crumbs(HostPlatform::Linux, "/a/b/c").expect("crumbs");
        assert_eq!(
            trail(&crumbs),
            [("/", "/"), ("a", "/a"), ("b", "/a/b"), ("c", "/a/b/c")]
        );
    }

    #[test]
    fn a_root_alone_is_its_own_only_crumb() {
        let crumbs = native_path_crumbs(HostPlatform::Linux, "/").expect("crumbs");
        assert_eq!(trail(&crumbs), [("/", "/")]);
    }

    #[test]
    fn a_windows_drive_trail_names_the_drive_root() {
        let crumbs = native_path_crumbs(HostPlatform::Windows, r"c:\a\b").expect("crumbs");
        assert_eq!(
            trail(&crumbs),
            [("C:/", "C:/"), ("a", "C:/a"), ("b", "C:/a/b")]
        );
        let drive_only = native_path_crumbs(HostPlatform::Windows, "C:/").expect("crumbs");
        assert_eq!(trail(&drive_only), [("C:/", "C:/")]);
    }

    #[test]
    fn a_unc_trail_starts_at_the_server_and_share() {
        let crumbs = native_path_crumbs(HostPlatform::Windows, r"\\host\share\a").expect("crumbs");
        assert_eq!(
            trail(&crumbs),
            [("//host/share", "//host/share"), ("a", "//host/share/a")]
        );
    }

    #[test]
    fn a_home_trail_starts_at_the_sentinel() {
        let crumbs = native_path_crumbs(HostPlatform::Linux, "~/proj").expect("crumbs");
        assert_eq!(trail(&crumbs), [("~", "~"), ("proj", "~/proj")]);
        let root = native_path_crumbs(HostPlatform::Linux, "~").expect("crumbs");
        assert_eq!(trail(&root), [("~", "~")]);
    }

    #[test]
    fn a_relative_path_still_has_a_basename() {
        assert_eq!(
            native_path_basename(HostPlatform::Linux, "proj/src/").as_deref(),
            Ok("src")
        );
        assert_eq!(
            native_path_basename(HostPlatform::Windows, r"proj\src").as_deref(),
            Ok("src")
        );
    }

    #[test]
    fn a_windows_basename_keeps_the_drive_colon() {
        assert_eq!(
            native_path_basename(HostPlatform::Windows, "C:/").as_deref(),
            Ok("C:")
        );
        assert_eq!(
            native_path_basename(HostPlatform::Windows, r"\\host\share").as_deref(),
            Ok("share")
        );
    }

    #[test]
    fn a_root_is_its_own_basename() {
        assert_eq!(
            native_path_basename(HostPlatform::Linux, "/").as_deref(),
            Ok("/")
        );
        assert_eq!(
            native_path_basename(HostPlatform::Linux, "~").as_deref(),
            Ok("~")
        );
    }

    #[test]
    fn a_parent_of_a_first_segment_is_the_root() {
        assert_eq!(
            native_path_dirname(HostPlatform::Linux, "/a").as_deref(),
            Ok("/")
        );
        assert_eq!(
            native_path_dirname(HostPlatform::Linux, "/a/b").as_deref(),
            Ok("/a")
        );
        assert_eq!(
            native_path_dirname(HostPlatform::Linux, "/").as_deref(),
            Ok("/")
        );
        assert_eq!(
            native_path_dirname(HostPlatform::Linux, "~/a").as_deref(),
            Ok("~")
        );
        assert_eq!(
            native_path_dirname(HostPlatform::Linux, "~").as_deref(),
            Ok("~")
        );
    }

    #[test]
    fn a_windows_parent_stops_at_the_drive_root() {
        assert_eq!(
            native_path_dirname(HostPlatform::Windows, "C:/a").as_deref(),
            Ok("C:/")
        );
        assert_eq!(
            native_path_dirname(HostPlatform::Windows, "C:/").as_deref(),
            Ok("C:/")
        );
        assert_eq!(
            native_path_dirname(HostPlatform::Windows, "//host/share").as_deref(),
            Ok("//host/share")
        );
        assert_eq!(
            native_path_dirname(HostPlatform::Windows, "//host/share/a").as_deref(),
            Ok("//host/share")
        );
    }

    #[test]
    fn a_join_takes_relative_parts_only() {
        assert_eq!(
            native_path_join(HostPlatform::Linux, "/a/b/", &["c", "../d"]).as_deref(),
            Ok("/a/b/d")
        );
        assert_eq!(
            native_path_join(HostPlatform::Windows, r"C:\a", &[r"b\c"]).as_deref(),
            Ok("C:/a/b/c")
        );
        assert_eq!(
            native_path_join(HostPlatform::Linux, "/", &["a"]).as_deref(),
            Ok("/a")
        );
    }

    #[test]
    fn an_absolute_or_drive_bearing_part_is_refused_by_name() {
        assert_eq!(
            native_path_join(HostPlatform::Linux, "/a", &["/b"]),
            Err(NativePathError::JoinPartNotRelative("/b".to_owned()))
        );
        assert_eq!(
            native_path_join(HostPlatform::Windows, "C:/a", &["D:b"]),
            Err(NativePathError::JoinPartNotRelative("D:b".to_owned()))
        );
    }
}
