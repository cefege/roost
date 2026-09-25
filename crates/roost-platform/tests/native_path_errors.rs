//! One test per refusal, on every platform it can happen on. Each of these
//! is a condition a caller has to handle differently, so a variant that stops
//! firing is a caller that starts guessing.

use roost_platform::{
    HostPlatform, NativePathError, native_path_to_fs_path, normalize_native_path,
};

#[test]
fn an_empty_path_is_refused() {
    for platform in HostPlatform::ALL {
        assert_eq!(
            normalize_native_path(platform, ""),
            Err(NativePathError::EmptyOrNul),
            "platform {platform}"
        );
    }
}

#[test]
fn a_nul_byte_in_a_path_is_refused_before_any_parsing() {
    for platform in HostPlatform::ALL {
        assert_eq!(
            normalize_native_path(platform, "/a\0b"),
            Err(NativePathError::EmptyOrNul),
            "platform {platform}"
        );
    }
}

#[test]
fn a_dot_dot_past_the_root_is_refused_rather_than_clamped() {
    assert_eq!(
        normalize_native_path(HostPlatform::Linux, "/.."),
        Err(NativePathError::EscapesRoot)
    );
    assert_eq!(
        normalize_native_path(HostPlatform::MacOs, "/a/../.."),
        Err(NativePathError::EscapesRoot)
    );
    assert_eq!(
        normalize_native_path(HostPlatform::Windows, "C:/.."),
        Err(NativePathError::EscapesRoot)
    );
    assert_eq!(
        normalize_native_path(HostPlatform::Windows, "//host/share/../.."),
        Err(NativePathError::EscapesRoot)
    );
    // One level below the root is still a real path.
    assert_eq!(
        normalize_native_path(HostPlatform::Linux, "/a/..").as_deref(),
        Ok("/")
    );
}

#[test]
fn a_relative_posix_path_is_refused_on_both_posix_platforms() {
    for platform in [HostPlatform::Linux, HostPlatform::MacOs] {
        assert_eq!(
            normalize_native_path(platform, "proj/src"),
            Err(NativePathError::PosixNotAbsolute("proj/src".to_owned())),
            "platform {platform}"
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
fn a_unc_path_without_a_server_and_share_is_refused() {
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
    // A bare `\\` is a UNC root with no server and no share.
    assert_eq!(
        normalize_native_path(HostPlatform::Windows, "\\\\"),
        Err(NativePathError::UncMissingServerShare("\\\\".to_owned()))
    );
    assert_eq!(
        normalize_native_path(HostPlatform::Windows, "relative/proj"),
        Err(NativePathError::WindowsNotDriveAbsoluteOrUnc(
            "relative/proj".to_owned()
        ))
    );
}

#[test]
fn a_home_sentinel_never_reaches_a_filesystem_call() {
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
