//! `roost self-link` is idempotent, repairs a link that points somewhere else,
//! and refuses to overwrite something that is not a link.
//!
//! The assertions are about what is on disk after the command and what an
//! operator is told when it refuses. Phase 7's cutover runs this unattended, so
//! a silent success on a machine whose link still pointed at an older
//! generation's install would be the worst outcome the command has.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use roost_cli::quickstart::self_link::{LinkOutcome, write_link};

/// A throwaway tree that removes itself. Each case gets its own, because two
/// cases sharing one would see each other's link.
struct TempTree {
    root: PathBuf,
}

impl TempTree {
    fn new(case: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-self-link-{}-{case}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(root.join(".local/bin")).expect("the throwaway tree is created");
        Self { root }
    }

    fn link(&self) -> PathBuf {
        self.root.join(".local/bin/roost")
    }

    /// A release program that exists, so the link has something real to point
    /// at. The content is irrelevant; only that the path is a file is.
    fn release(&self, name: &str) -> PathBuf {
        let path = self.root.join(format!("versions/{name}/bin/roost"));
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("the release dir exists");
        std::fs::write(&path, b"#!/bin/sh\nexec roost \"$@\"\n").expect("the program is written");
        path
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_missing_link_is_created_and_a_second_run_changes_nothing() {
    let tree = TempTree::new("idempotent");
    let target = tree.release("v3");
    let link = tree.link();

    let first = write_link(&link, &target).expect("the first run creates the link");
    assert_eq!(first, LinkOutcome::Created);
    assert_eq!(
        std::fs::read_link(&link).expect("the link is readable"),
        target
    );

    let second = write_link(&link, &target).expect("the second run succeeds");
    assert_eq!(
        second,
        LinkOutcome::AlreadyCorrect,
        "a second run on a correct link must change nothing"
    );
    assert_eq!(
        std::fs::read_link(&link).expect("the link is readable"),
        target,
        "the link still points at the release"
    );
    let staged = list_staging_files(&tree.root.join(".local/bin"));
    assert!(
        staged.is_empty(),
        "an idempotent run leaves no staging file behind: {staged:?}"
    );
}

#[test]
fn a_link_that_still_points_at_an_older_install_is_repointed_and_the_old_target_is_said() {
    let tree = TempTree::new("v2");
    let current = tree.release("v3");
    // A v2 release: a different root, a different generation, and the case
    // that silently keeps serving v2 while the operator believes they cut over.
    let older = tree.release("v2");
    let link = tree.link();
    std::os::unix::fs::symlink(&older, &link).expect("the stale link is written");

    let outcome = write_link(&link, &current).expect("the stale link is repaired");
    assert_eq!(
        outcome,
        LinkOutcome::Repaired {
            previous: Some(older.clone())
        }
    );
    assert_eq!(
        std::fs::read_link(&link).expect("the link is readable"),
        current,
        "the link now points at this install's release"
    );
    let said = outcome.sentence(&link, &current);
    assert!(
        said.contains(&older.display().to_string()),
        "the operator is told what the link pointed at before: {said}"
    );
    assert!(said.contains("was "), "{said}");
}

#[test]
fn a_broken_link_is_repaired_and_reported_as_having_had_no_target() {
    let tree = TempTree::new("broken");
    let current = tree.release("v3");
    let link = tree.link();
    std::os::unix::fs::symlink(tree.root.join("gone/roost"), &link).expect("the link is written");

    let outcome = write_link(&link, &current).expect("a broken link is repaired");
    assert_eq!(outcome, LinkOutcome::Repaired { previous: None });
    assert_eq!(
        std::fs::read_link(&link).expect("the link is readable"),
        current
    );
    assert!(
        outcome.sentence(&link, &current).contains("broken link"),
        "{}",
        outcome.sentence(&link, &current)
    );
}

#[test]
fn a_real_file_is_refused_and_its_contents_are_untouched() {
    let tree = TempTree::new("file");
    let current = tree.release("v3");
    let link = tree.link();
    let body = "#!/bin/sh\necho an operator wrote this\n";
    std::fs::write(&link, body).expect("the hand-written file is in place");

    let failure = write_link(&link, &current).expect_err("a real file is not a link");
    assert_eq!(failure.code, 1);
    assert!(
        failure.message.contains("not a symlink"),
        "the refusal says what it found: {failure}"
    );
    assert!(
        failure.message.contains(&format!("rm {}", link.display())),
        "the refusal says exactly what to remove: {failure}"
    );
    assert_eq!(
        std::fs::read_to_string(&link).expect("the file is readable"),
        body,
        "a refused repair must not have written to it"
    );
}

#[test]
fn a_directory_is_refused_and_still_there() {
    let tree = TempTree::new("dir");
    let current = tree.release("v3");
    let link = tree.link();
    std::fs::create_dir_all(link.join("inner")).expect("the directory is in place");

    let failure = write_link(&link, &current).expect_err("a directory is not a link");
    assert_eq!(failure.code, 1);
    assert!(failure.message.contains("not a symlink"), "{failure}");
    assert!(
        link.join("inner").is_dir(),
        "a refused repair must not have removed it"
    );
}

#[test]
fn a_release_that_is_not_installed_is_refused_by_the_path_it_looked_for() {
    let tree = TempTree::new("absent");
    let link = tree.link();
    let missing = tree.root.join("versions/never-installed/bin/roost");

    let failure = write_link(&link, &missing).expect_err("a dangling link is refused");
    assert_eq!(failure.code, 1);
    assert!(
        failure.message.contains(&missing.display().to_string()),
        "the refusal names the path it looked for: {failure}"
    );
    assert!(
        std::fs::symlink_metadata(&link).is_err(),
        "a refused run leaves no link behind"
    );
}

/// Every entry in a directory that is not a symlink, which is what an
/// interrupted replace would leave behind.
fn list_staging_files(directory: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("staged"))
        })
        .collect()
}
