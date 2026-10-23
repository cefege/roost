//! Deterministic Git fixtures for checkout reconciliation, clone fallback, and hooks.
//! These exercise the filesystem and Git state shared by the reusable backend.
//! Each fixture owns both the source repository and every linked destination.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::{CloneMode, Error, WorktreeAddOptions, worktree_add};
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn sibling(root: &Path, name: &str) -> std::path::PathBuf {
    root.parent().unwrap().join(format!(
        "{}-{name}",
        root.file_name().unwrap().to_string_lossy()
    ))
}

fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    fs::create_dir(&root).unwrap();
    git(&root, &["init", "-b", "main"]);
    git(&root, &["config", "user.name", "Roost test"]);
    git(&root, &["config", "user.email", "roost@example.invalid"]);
    fs::write(root.join("tracked"), "base\n").unwrap();
    fs::write(root.join(".gitignore"), "build/\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-m", "base"]);
    git(&root, &["branch", "target"]);
    git(&root, &["checkout", "target"]);
    fs::write(root.join("tracked"), "target\n").unwrap();
    git(&root, &["commit", "-am", "target"]);
    git(&root, &["checkout", "main"]);
    temp
}

mod safety;

#[test]
fn target_commit_reconciliation_preserves_ignored_files_and_source_state() {
    let temp = fixture();
    let root = &temp.path().join("source");
    fs::write(root.join("tracked"), "dirty source\n").unwrap();
    fs::write(root.join("untracked"), "source only\n").unwrap();
    fs::create_dir_all(root.join("build")).unwrap();
    fs::write(root.join("build/output"), "ignored artifact\n").unwrap();
    let before = git(root, &["status", "--porcelain"]);
    let linked = sibling(root, "target-worktree");
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
    assert_eq!(
        fs::read_to_string(linked.join("tracked")).unwrap(),
        "target\n"
    );
    assert!(!linked.join("untracked").exists());
    assert_eq!(
        fs::read_to_string(root.join("tracked")).unwrap(),
        "dirty source\n"
    );
    assert_eq!(git(root, &["status", "--porcelain"]), before);
    assert_eq!(git(&linked, &["symbolic-ref", "HEAD"]), "refs/heads/target");
    if result.cloned {
        assert_eq!(
            fs::read_to_string(linked.join("build/output")).unwrap(),
            "ignored artifact\n"
        );
        fs::write(linked.join("build/output"), "clone artifact edit\n").unwrap();
        assert_eq!(
            fs::read_to_string(root.join("build/output")).unwrap(),
            "ignored artifact\n"
        );
    } else {
        assert!(result.clone_error.is_some());
    }
    fs::write(linked.join("tracked"), "clone changed\n").unwrap();
    assert_eq!(
        fs::read_to_string(root.join("tracked")).unwrap(),
        "dirty source\n"
    );
    let _ = Command::new("git")
        .current_dir(root)
        .args(["worktree", "remove", "--force"])
        .arg(&linked)
        .status();
    git(root, &["branch", "same-head", "main"]);
    let same_head = sibling(root, "same-head-worktree");
    worktree_add(
        root,
        &same_head,
        "same-head",
        WorktreeAddOptions {
            detach: false,
            clone: CloneMode::Auto,
            keep_changes: false,
        },
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(same_head.join("tracked")).unwrap(),
        "base\n"
    );
    assert!(!same_head.join("untracked").exists());
    assert_eq!(git(&same_head, &["status", "--porcelain"]), "");
    assert_eq!(git(root, &["status", "--porcelain"]), before);
    let _ = Command::new("git")
        .current_dir(root)
        .args(["worktree", "remove", "--force"])
        .arg(&same_head)
        .status();
}

#[test]
fn keep_changes_retains_staged_unstaged_and_untracked_state_at_source_head() {
    for clone in [CloneMode::Auto, CloneMode::Off] {
        let temp = fixture();
        let root = &temp.path().join("source");
        fs::write(root.join("tracked"), "staged\n").unwrap();
        git(root, &["add", "tracked"]);
        fs::write(root.join("tracked"), "staged and edited\n").unwrap();
        fs::write(root.join("new-file"), "untracked\n").unwrap();
        let source_status = git(root, &["status", "--porcelain"]);
        let source_index = fs::read(root.join(".git/index")).unwrap();
        let linked = sibling(root, &format!("kept-worktree-{clone:?}"));
        worktree_add(
            root,
            &linked,
            "HEAD",
            WorktreeAddOptions {
                detach: false,
                clone,
                keep_changes: true,
            },
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(linked.join("tracked")).unwrap(),
            "staged and edited\n"
        );
        assert_eq!(
            fs::read_to_string(linked.join("new-file")).unwrap(),
            "untracked\n"
        );
        assert_eq!(git(&linked, &["status", "--porcelain"]), source_status);
        assert_eq!(git(root, &["status", "--porcelain"]), source_status);
        assert_eq!(fs::read(root.join(".git/index")).unwrap(), source_index);
        let _ = Command::new("git")
            .current_dir(root)
            .args(["worktree", "remove", "--force"])
            .arg(&linked)
            .status();
    }
}
