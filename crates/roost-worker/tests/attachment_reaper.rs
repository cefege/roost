#![cfg(unix)]
//! The attachment reaper's policy: files past the TTL go, the base stays under
//! 1 GiB by evicting the oldest survivors, the dedup manifest is never swept,
//! and emptied directories are removed. v2 has no reaper test; these pin the
//! behaviour of `apps/worker/src/attachments/attachment-reaper.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod attachment_support;

use std::fs::File;
use std::path::Path;
use std::time::{Duration, SystemTime};

use attachment_support::Scratch;
use roost_worker::attachments::reaper::{
    ATTACHMENT_SIZE_CAP_BYTES, ATTACHMENT_TTL, sweep_attachments,
};

const HOUR: Duration = Duration::from_secs(60 * 60);

fn file_aged(path: &Path, bytes: u64, age: Duration) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = File::create(path).unwrap();
    // Sparse: the length counts against the cap without using the disk.
    file.set_len(bytes).unwrap();
    file.set_modified(SystemTime::now() - age).unwrap();
}

#[test]
fn files_past_the_ttl_go_and_the_manifest_and_fresh_files_stay() {
    let scratch = Scratch::new("reaper-ttl");
    let base = scratch.base();
    let session = base.session_dir("session-a");
    let stale = ATTACHMENT_TTL + HOUR;
    file_aged(&session.join("old.txt"), 3, stale);
    file_aged(&session.join("new.txt"), 3, HOUR);
    file_aged(&session.join(".roost-manifest.json"), 2, stale);
    file_aged(&session.join(".operations").join("up.json"), 2, stale);
    file_aged(&session.join(".operations").join("live.part"), 2, HOUR);
    // Outside the session, so the order the sweep visits entries in cannot
    // delete the target before its shortcut is judged.
    let target = scratch.root.join("shortcut-target.bin");
    file_aged(&target, 1, stale);
    std::fs::create_dir_all(session.join(".shortcuts")).unwrap();
    std::os::unix::fs::symlink(&target, session.join(".shortcuts").join("p1")).unwrap();
    file_aged(
        &base.session_dir("session-b").join("only-old.bin"),
        1,
        stale,
    );

    let summary = sweep_attachments(&base, SystemTime::now()).unwrap();

    assert!(!session.join("old.txt").exists());
    assert!(session.join("new.txt").exists());
    assert!(
        session.join(".roost-manifest.json").exists(),
        "the manifest is never swept"
    );
    assert!(!session.join(".operations").join("up.json").exists());
    assert!(session.join(".operations").join("live.part").exists());
    assert!(
        !session.join(".shortcuts").exists(),
        "a shortcut ages with its target and its emptied directory goes"
    );
    assert!(
        target.exists(),
        "only the link is removed, never what it points at"
    );
    assert!(
        !base.session_dir("session-b").exists(),
        "an emptied session directory is removed"
    );
    assert_eq!(summary.evicted, 0);
}

#[test]
fn the_base_is_held_under_the_cap_by_evicting_the_oldest_survivors() {
    let scratch = Scratch::new("reaper-cap");
    let base = scratch.base();
    let half = ATTACHMENT_SIZE_CAP_BYTES / 2;
    let oldest = base.session_dir("session-a").join("oldest.bin");
    let middle = base
        .session_dir("session-b")
        .join(".operations")
        .join("upload.part");
    let newest = base.session_dir("session-a").join("newest.bin");
    file_aged(&oldest, half, 3 * HOUR);
    file_aged(&middle, half, 2 * HOUR);
    file_aged(&newest, half, HOUR);

    let summary = sweep_attachments(&base, SystemTime::now()).unwrap();

    assert!(
        !oldest.exists(),
        "the least recently modified file is evicted first"
    );
    assert!(
        middle.exists() && newest.exists(),
        "eviction stops once the total fits"
    );
    assert_eq!((summary.evicted, summary.retained_bytes), (1, 2 * half));
}

#[test]
fn a_base_that_does_not_exist_yet_is_a_quiet_no_op() {
    let scratch = Scratch::new("reaper-none");
    let summary = sweep_attachments(&scratch.base(), SystemTime::now()).unwrap();
    assert_eq!(summary.expired + summary.evicted, 0);
}
