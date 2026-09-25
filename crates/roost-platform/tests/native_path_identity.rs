//! Path identity folds only what the host filesystem itself treats as one
//! path: Windows case, and darwin's three system symlinks (`/tmp`, `/var`,
//! `/etc` ARE `/private/{tmp,var,etc}`). Display must never be folded — that
//! is normalize's job — so these assertions pin identity only.

use roost_platform::{
    DARWIN_PRIVATE_ROOTS, HostPlatform, native_path_identity_key, same_worker_folder,
};

#[test]
fn darwin_folds_its_system_symlinks_so_one_directory_has_one_key() {
    for root in DARWIN_PRIVATE_ROOTS {
        assert_eq!(
            native_path_identity_key(HostPlatform::MacOs, &format!("/{root}/proj")).as_deref(),
            Ok(format!("/private/{root}/proj").as_str())
        );
        // The bare root folds too: a session spawned at /tmp reports /private/tmp.
        assert_eq!(
            native_path_identity_key(HostPlatform::MacOs, &format!("/{root}")).as_deref(),
            Ok(format!("/private/{root}").as_str())
        );
    }
}

#[test]
fn the_fold_is_exact_not_a_private_prefix_heuristic() {
    // /private/other is a real directory, distinct from /other.
    assert_ne!(
        native_path_identity_key(HostPlatform::MacOs, "/private/other").as_deref(),
        native_path_identity_key(HostPlatform::MacOs, "/other").as_deref()
    );
    // A path that merely starts with a root name is untouched.
    assert_ne!(
        native_path_identity_key(HostPlatform::MacOs, "/tmpfiles").as_deref(),
        native_path_identity_key(HostPlatform::MacOs, "/private/tmpfiles").as_deref()
    );
    // A folded root and its `/private` spelling are the SAME directory, so
    // they share a key — that is the whole point of the fold. The heuristic it
    // must NOT be is a blanket `/private` strip, which is what the two
    // assertions above pin: `/private/other` and `/tmpfiles` stay distinct.
    assert_eq!(
        native_path_identity_key(HostPlatform::MacOs, "/var/lib").as_deref(),
        native_path_identity_key(HostPlatform::MacOs, "/private/var/lib").as_deref()
    );
}

#[test]
fn linux_has_no_such_symlinks_so_the_same_pair_stays_distinct() {
    assert_ne!(
        native_path_identity_key(HostPlatform::Linux, "/tmp/proj").as_deref(),
        native_path_identity_key(HostPlatform::Linux, "/private/tmp/proj").as_deref()
    );
}

#[test]
fn windows_folds_case_because_the_filesystem_does() {
    assert_eq!(
        native_path_identity_key(HostPlatform::Windows, r"C:\Users\Me\Proj").as_deref(),
        Ok("c:/users/me/proj")
    );
    assert_eq!(
        native_path_identity_key(HostPlatform::Windows, r"c:\users\me\proj").as_deref(),
        Ok("c:/users/me/proj")
    );
}

#[test]
fn same_worker_folder_folds_a_known_os_and_falls_back_to_equality_otherwise() {
    assert!(same_worker_folder(
        "darwin",
        "/tmp/proj",
        "/private/tmp/proj"
    ));
    assert!(!same_worker_folder(
        "linux",
        "/tmp/proj",
        "/private/tmp/proj"
    ));
    // Unknown or absent os: exact equality, never a false merge.
    assert!(!same_worker_folder("", "/tmp/proj", "/private/tmp/proj"));
    assert!(!same_worker_folder(
        "plan9",
        "/tmp/proj",
        "/private/tmp/proj"
    ));
    assert!(same_worker_folder("plan9", "/tmp/proj", "/tmp/proj"));
    // A path neither side can normalize also falls back instead of throwing.
    assert!(same_worker_folder(
        "darwin",
        "relative/proj",
        "relative/proj"
    ));
    assert!(!same_worker_folder("darwin", "relative/proj", "/tmp/proj"));
    assert!(!same_worker_folder("win32", "C:relative", "c:RELATIVE"));
}
