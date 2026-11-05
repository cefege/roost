//! Historical file reads are verified against real Git repositories.
//! HEAD, the index and the worktree deliberately contain different bytes;
//! missing parents and unusual path names must not fall back to disk reads.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use std::fs;
use std::io::Write;
use std::process::Command as StdCommand;
use tempfile::TempDir;

fn git(root: &Path, args: &[&str], input: Option<&[u8]>) -> Vec<u8> {
    let mut command = StdCommand::new("git");
    command
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1");
    if let Some(input) = input {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("git starts");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(input)
            .expect("input writes");
        let output = child.wait_with_output().expect("git exits");
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    } else {
        let output = command.output().expect("git starts");
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }
}

fn repository() -> TempDir {
    let directory = tempfile::tempdir().expect("temporary directory");
    git(directory.path(), &["init", "-q"], None);
    git(
        directory.path(),
        &["config", "user.name", "Roost Test"],
        None,
    );
    git(
        directory.path(),
        &["config", "user.email", "roost@example.invalid"],
        None,
    );
    directory
}

#[tokio::test]
async fn reads_head_index_binary_and_deleted_paths_without_using_disk_bytes() {
    let directory = repository();
    let name = if cfg!(windows) {
        "name with spaces.bin"
    } else {
        "name with\nnewline.bin"
    };
    let path = directory.path().join(name);
    let head = [0, 1, 255, b'h'];
    let staged = [0, 2, 254, b'i'];
    fs::write(&path, head).expect("head fixture writes");
    git(directory.path(), &["add", "--", name], None);
    git(directory.path(), &["commit", "-qm", "seed"], None);
    fs::write(&path, staged).expect("staged fixture writes");
    git(directory.path(), &["add", "--", name], None);
    fs::write(&path, b"working tree").expect("worktree fixture writes");
    assert_eq!(
        read_git_file(path.to_str().expect("utf8 path"), "HEAD")
            .await
            .expect("HEAD blob"),
        head.to_vec()
    );
    assert_eq!(
        read_git_file(path.to_str().expect("utf8 path"), ":0")
            .await
            .expect("index blob"),
        staged.to_vec()
    );

    let deleted = directory.path().join("deleted.txt");
    fs::write(&deleted, b"still in HEAD").expect("deleted fixture writes");
    git(directory.path(), &["add", "--", "deleted.txt"], None);
    git(directory.path(), &["commit", "-qm", "deleted seed"], None);
    fs::remove_file(&deleted).expect("working-tree path removed");
    assert_eq!(
        read_git_file(deleted.to_str().expect("utf8 path"), "HEAD")
            .await
            .expect("deleted path blob"),
        b"still in HEAD"
    );
}

#[tokio::test]
async fn refuses_missing_objects_and_oversized_blobs() {
    let directory = repository();
    let path = directory.path().join("small.bin");
    fs::write(&path, b"small").expect("fixture writes");
    git(directory.path(), &["add", "--", "small.bin"], None);
    git(directory.path(), &["commit", "-qm", "seed"], None);
    assert!(
        read_git_file(path.to_str().expect("utf8 path"), "not-a-revision")
            .await
            .is_err()
    );
    let missing = directory.path().join("missing.txt");
    assert!(
        read_git_file(missing.to_str().expect("utf8 path"), "HEAD")
            .await
            .is_err()
    );

    let oversized = vec![b'x'; READ_FILE_MAX_BYTES as usize + 1];
    let oid = String::from_utf8(git(
        directory.path(),
        &["hash-object", "-w", "--stdin"],
        Some(&oversized),
    ))
    .expect("object id is utf8");
    let cacheinfo = format!("100644,{},large.bin", oid.trim());
    git(
        directory.path(),
        &["update-index", "--add", "--cacheinfo", &cacheinfo],
        None,
    );
    let large_path = directory.path().join("large.bin");
    assert!(
        read_git_file(large_path.to_str().expect("utf8 path"), ":0")
            .await
            .is_err()
    );
}

#[cfg(not(windows))]
#[tokio::test]
async fn reads_deleted_nested_files_from_a_repository_whose_name_ends_in_a_newline() {
    let temporary = tempfile::tempdir().expect("temporary root");
    let root = temporary.path().join("repo with spaces\n");
    fs::create_dir(&root).expect("repo directory");
    git(&root, &["init", "-q"], None);
    git(&root, &["config", "user.name", "Roost Test"], None);
    git(
        &root,
        &["config", "user.email", "roost@example.invalid"],
        None,
    );
    let directory = root.join("deleted/nested");
    fs::create_dir_all(&directory).expect("nested directory");
    let path = directory.join("historical.txt");
    fs::write(&path, b"committed bytes").expect("fixture");
    git(&root, &["add", "--", "deleted/nested/historical.txt"], None);
    git(&root, &["commit", "-qm", "seed"], None);
    fs::remove_dir_all(root.join("deleted")).expect("remove all parents");
    assert_eq!(
        read_git_file(path.to_str().expect("path"), "HEAD")
            .await
            .expect("historical file"),
        b"committed bytes"
    );
}

#[tokio::test]
async fn index_lookup_rejects_suffix_matches_directories_and_unmerged_stages() {
    let directory = repository();
    let root = directory.path();
    fs::create_dir(root.join("nested")).expect("directory");
    fs::write(
        root.join("nested/exact.txt"),
        b"not the requested root path",
    )
    .expect("fixture");
    git(root, &["add", "--", "nested/exact.txt"], None);
    for name in ["exact.txt", "nested"] {
        assert!(
            read_git_file(root.join(name).to_str().expect("path"), ":0")
                .await
                .is_err()
        );
    }
    let first = String::from_utf8(git(root, &["hash-object", "-w", "--stdin"], Some(b"ours")))
        .expect("oid");
    let second = String::from_utf8(git(
        root,
        &["hash-object", "-w", "--stdin"],
        Some(b"theirs"),
    ))
    .expect("oid");
    let conflict = format!(
        "100644 {} 1\tconflict.txt\n100644 {} 2\tconflict.txt\n",
        first.trim(),
        second.trim()
    );
    git(
        root,
        &["update-index", "--index-info"],
        Some(conflict.as_bytes()),
    );
    assert!(
        read_git_file(root.join("conflict.txt").to_str().expect("path"), ":0")
            .await
            .is_err()
    );
    let traversal = root.join("missing/../../outside.txt");
    assert!(
        read_git_file(traversal.to_str().expect("path"), "HEAD")
            .await
            .is_err()
    );
}
