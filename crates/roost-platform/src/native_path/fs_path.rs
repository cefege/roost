//! The one place a canonical path becomes something an OS call takes. The
//! browse sentinel is refused here: a caller holding `~/proj` has not decided
//! which user's home it means, and guessing is how a machine ends up writing
//! into the wrong tree.

use crate::host_platform::HostPlatform;
use crate::native_path::normalize::{NativePathError, normalize_native_path};

/// The canonical path as the host's own API wants it: `/` separators on
/// POSIX, `\` on Windows.
pub fn native_path_to_fs_path(
    platform: HostPlatform,
    path: &str,
) -> Result<String, NativePathError> {
    let normalized = normalize_native_path(platform, path)?;
    if normalized == "~" || normalized.starts_with("~/") {
        return Err(NativePathError::HomeSentinelUnresolvedForFilesystem);
    }
    Ok(match platform {
        HostPlatform::MacOs | HostPlatform::Linux => normalized,
        HostPlatform::Windows => normalized.replace('/', "\\"),
    })
}

#[cfg(test)]
mod tests {
    use super::native_path_to_fs_path;
    use crate::host_platform::HostPlatform;
    use crate::native_path::normalize::NativePathError;

    #[test]
    fn a_posix_path_keeps_its_separators() {
        assert_eq!(
            native_path_to_fs_path(HostPlatform::Linux, "/a//b/./c").as_deref(),
            Ok("/a/b/c")
        );
    }

    #[test]
    fn a_windows_path_comes_back_with_backslashes() {
        assert_eq!(
            native_path_to_fs_path(HostPlatform::Windows, "C:/a/b").as_deref(),
            Ok(r"C:\a\b")
        );
    }

    #[test]
    fn the_home_sentinel_must_be_resolved_before_a_filesystem_call() {
        for platform in HostPlatform::ALL {
            assert_eq!(
                native_path_to_fs_path(platform, "~"),
                Err(NativePathError::HomeSentinelUnresolvedForFilesystem)
            );
            assert_eq!(
                native_path_to_fs_path(platform, "~/proj"),
                Err(NativePathError::HomeSentinelUnresolvedForFilesystem)
            );
        }
    }
}
