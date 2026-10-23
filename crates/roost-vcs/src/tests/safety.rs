//! Destination, hook, and clone-fallback behavior against real Git repositories.
//! The parent module supplies per-test repositories and subprocess fixtures.
//! All linked destinations remain inside the owning temporary directory.

use super::*;
#[cfg(unix)]
#[test]
fn post_checkout_hook_runs_once_and_failure_keeps_created_worktree() {
    use std::os::unix::fs::PermissionsExt;
    for detach in [false, true] {
        let temp = fixture();
        let root = &temp.path().join("source");
        let hooks = root.join("hooks");
        fs::create_dir(&hooks).unwrap();
        let hook = hooks.join("post-checkout");
        fs::write(&hook, "#!/bin/sh\nprintf 'called\\n' >> hook-count\nprintf '%s\\n' \"$@\" > hook-args\nexit 7\n").unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        git(root, &["config", "core.hooksPath", hooks.to_str().unwrap()]);
        let linked = sibling(root, "hook-worktree");
        let result = worktree_add(
            root,
            &linked,
            "target",
            WorktreeAddOptions {
                detach,
                clone: CloneMode::Off,
                keep_changes: false,
            },
        )
        .unwrap();
        assert!(!linked.join("hook-count").exists());
        let hook = crate::run_post_checkout_hook(&linked, &result.head).unwrap();
        assert_eq!(hook.status.code(), Some(7));
        assert!(linked.join(".git").exists());
        assert_eq!(
            fs::read_to_string(linked.join("hook-count")).unwrap(),
            "called\n"
        );
        assert_eq!(
            fs::read_to_string(linked.join("hook-args")).unwrap(),
            format!("{}\n{}\n1\n", "0".repeat(result.head.len()), result.head)
        );
        assert!(!root.join("hook-count").exists());
    }
}

#[cfg(unix)]
#[test]
fn target_checkout_preserves_git_symlink_and_executable_modes() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = fixture();
    let root = &temp.path().join("source");
    git(root, &["checkout", "target"]);
    fs::write(root.join("executable"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(root.join("executable"), fs::Permissions::from_mode(0o755)).unwrap();
    symlink("tracked", root.join("tracked-link")).unwrap();
    git(root, &["add", "executable", "tracked-link"]);
    git(root, &["commit", "-m", "modes"]);
    git(root, &["checkout", "main"]);
    let linked = sibling(root, "mode-worktree");
    worktree_add(
        root,
        &linked,
        "target",
        WorktreeAddOptions {
            detach: false,
            clone: CloneMode::Auto,
            keep_changes: false,
        },
    )
    .unwrap();
    assert!(
        fs::symlink_metadata(linked.join("tracked-link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::metadata(linked.join("executable"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    let _ = Command::new("git")
        .current_dir(root)
        .args(["worktree", "remove", "--force"])
        .arg(&linked)
        .status();
}

#[cfg(unix)]
#[test]
fn unsupported_clone_input_falls_back_to_a_real_git_checkout() {
    use std::os::unix::net::UnixListener;
    let temp = fixture();
    let root = &temp.path().join("source");
    let socket = root.join("untracked-socket");
    let listener = UnixListener::bind(&socket).unwrap();
    let linked = sibling(root, "fallback-worktree");
    let result = worktree_add(
        root,
        &linked,
        "target",
        WorktreeAddOptions {
            detach: false,
            clone: CloneMode::Auto,
            keep_changes: false,
        },
    )
    .unwrap();
    assert!(!result.cloned);
    assert!(result.clone_error.is_some());
    assert_eq!(
        fs::read_to_string(linked.join("tracked")).unwrap(),
        "target\n"
    );
    assert!(!linked.join("untracked-socket").exists());

    let _ = Command::new("git")
        .current_dir(root)
        .args(["worktree", "remove", "--force"])
        .arg(&linked)
        .status();
    drop(listener);
    fs::remove_file(socket).unwrap();
}
#[test]
fn unsafe_destinations_and_non_head_keep_requests_are_rejected_before_creation() {
    let temp = fixture();
    let root = &temp.path().join("source");
    let occupied = sibling(root, "occupied-worktree");
    fs::create_dir(&occupied).unwrap();
    fs::write(occupied.join("keep"), "untouched\n").unwrap();
    assert!(matches!(
        worktree_add(
            root,
            &occupied,
            "target",
            WorktreeAddOptions {
                detach: false,
                clone: CloneMode::Auto,
                keep_changes: false,
            }
        ),
        Err(Error::Git(_))
    ));
    assert_eq!(
        fs::read_to_string(occupied.join("keep")).unwrap(),
        "untouched\n"
    );
    fs::remove_dir_all(&occupied).unwrap();

    let rejected = sibling(root, "wrong-head-worktree");
    let error = worktree_add(
        root,
        &rejected,
        "target",
        WorktreeAddOptions {
            detach: true,
            clone: CloneMode::Off,
            keep_changes: true,
        },
    )
    .unwrap_err();
    assert!(matches!(error, Error::KeepChangesAtOtherCommit));
    assert!(!rejected.exists());
}

#[test]
fn failed_registration_preserves_an_existing_prunable_worktree() {
    let temp = fixture();
    let root = &temp.path().join("source");
    let linked = sibling(root, "registered-worktree");
    worktree_add(
        root,
        &linked,
        "target",
        WorktreeAddOptions {
            detach: false,
            clone: CloneMode::Off,
            keep_changes: false,
        },
    )
    .unwrap();
    for entry in fs::read_dir(&linked).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            fs::remove_dir_all(path).unwrap();
        } else {
            fs::remove_file(path).unwrap();
        }
    }
    let registrations = git(root, &["worktree", "list", "--porcelain"]);
    let result = worktree_add(
        root,
        &linked,
        "HEAD",
        WorktreeAddOptions {
            detach: true,
            clone: CloneMode::Auto,
            keep_changes: false,
        },
    );
    assert!(matches!(result, Err(Error::Git(_))));
    assert_eq!(
        git(root, &["worktree", "list", "--porcelain"]),
        registrations
    );
    assert!(linked.is_dir());
    assert!(fs::read_dir(&linked).unwrap().next().is_none());
}
