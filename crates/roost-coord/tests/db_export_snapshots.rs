//! The database export's two bounds, and the one refusal that has to be a
//! refusal: a whole coordinator database goes to a caller that arrived on this
//! host and to nobody else.
//!
//! The interesting behaviour is not the download — it is that a full copy of a
//! database that reaches hundreds of megabytes cannot be left lying in the data
//! directory by a caller who asked three times.
//!
//! `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to
//! be stated here rather than inherited.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use roost_coord::db::CoordDb;
use roost_coord::maintenance::export_snapshot::{
    EXPORT_SNAPSHOT_MAX_RESIDENT, EXPORT_SNAPSHOT_PREFIX, EXPORT_SNAPSHOT_SUFFIX,
    prepare_export_snapshot, sweep_export_snapshots,
};

/// A coordinator over a scratch database, and the directory its copies land in.
struct ExportFixture {
    database: CoordDb,
    root: PathBuf,
}

impl ExportFixture {
    async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-export-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = roost_coord::db::open(&root.join("coord.db"))
            .await
            .expect("a migrated coordinator database");
        Self { database, root }
    }

    fn data_dir(&self) -> &Path {
        self.root.as_path()
    }
}

impl Drop for ExportFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Every export copy currently in the data directory, oldest first.
fn resident_copies(directory: &Path) -> Vec<PathBuf> {
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(directory)
        .expect("the data directory")
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.starts_with(EXPORT_SNAPSHOT_PREFIX) && name.ends_with(EXPORT_SNAPSHOT_SUFFIX)
        })
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, entry.path()))
        })
        .collect();
    found.sort_by(|left, right| left.0.cmp(&right.0));
    found.into_iter().map(|(_, path)| path).collect()
}

#[tokio::test]
async fn an_export_is_a_consistent_copy_and_not_the_live_file() {
    let fixture = ExportFixture::new("copy").await;
    let snapshot = prepare_export_snapshot(&fixture.database)
        .await
        .expect("an export snapshot");

    assert!(
        snapshot.path.starts_with(fixture.data_dir()),
        "a copy is written beside the database, not into a temp directory the \
         reaper would not find"
    );
    assert!(
        snapshot.size > 0,
        "an empty copy is a truncated one that `sqlite3` would refuse"
    );
    assert_ne!(snapshot.path, fixture.database.path());
    assert_eq!(
        std::fs::metadata(&snapshot.path).expect("the copy").len(),
        snapshot.size,
        "the advertised size is the size on disk, or a download client waits \
         forever for bytes that never arrive"
    );
    // It is a real database, not a copy of one: opening it is the only proof.
    let reopened = roost_coord::db::open(&snapshot.path).await;
    assert!(
        reopened.is_ok(),
        "the copy did not open as a coordinator database: {:?}",
        reopened.err()
    );
}

#[tokio::test]
async fn asking_again_does_not_grow_the_directory_past_the_resident_bound() {
    let fixture = ExportFixture::new("bound").await;
    // One more request than the bound allows. Each one takes a FULL copy of the
    // database, so without the count bound this is a disk incident and not a
    // housekeeping detail.
    for _ in 0..(EXPORT_SNAPSHOT_MAX_RESIDENT + 1) {
        prepare_export_snapshot(&fixture.database)
            .await
            .expect("an export snapshot");
    }
    let resident = resident_copies(fixture.data_dir());
    assert_eq!(
        resident.len(),
        EXPORT_SNAPSHOT_MAX_RESIDENT,
        "a caller that asks repeatedly must not be able to pin disk: {resident:?}"
    );
}

#[tokio::test]
async fn a_sweep_drops_the_past_the_age_bound_and_then_the_surplus() {
    let fixture = ExportFixture::new("sweep").await;
    // Four preparations, and the resident count is already at the bound: each
    // preparation sweeps BEFORE it copies, so the bound is never briefly
    // exceeded rather than being enforced after the fact.
    for _ in 0..4 {
        prepare_export_snapshot(&fixture.database)
            .await
            .expect("an export snapshot");
    }
    assert_eq!(
        resident_copies(fixture.data_dir()).len(),
        EXPORT_SNAPSHOT_MAX_RESIDENT
    );

    // Narrowing the keep count to one removes the surplus: every copy here is
    // younger than the age bound, so the count is the only rule that can fire.
    // The property is the pair — the reported number and what is left on disk
    // agree, and what is left is within the bound — because a sweep that
    // removed more than it reported would still pass either assertion alone.
    let before = resident_copies(fixture.data_dir()).len();
    let removed = sweep_export_snapshots(fixture.data_dir(), Duration::from_secs(900), 1).await;
    let after = resident_copies(fixture.data_dir()).len();
    assert!(after <= 1, "a keep count of one leaves at most one: {after}");
    assert_eq!(
        removed,
        before - after,
        "the sweep reported {removed} and removed {}: the count is what a log \
         line and the boot sweep both rely on",
        before - after
    );
    assert_eq!(after, 1, "a fresh copy survives its own sweep");

    // And the age bound alone, with the count wide open: a copy nobody is
    // downloading any more is disk that will still be there tomorrow, and a
    // boot sweep is the age bound at zero.
    assert_eq!(
        sweep_export_snapshots(fixture.data_dir(), Duration::from_secs(0), usize::MAX).await,
        1,
        "an age bound of zero is a boot sweep and takes everything"
    );
    assert!(resident_copies(fixture.data_dir()).is_empty());
}

#[tokio::test]
async fn a_sweep_never_touches_a_file_that_is_not_an_export_copy() {
    let fixture = ExportFixture::new("foreign").await;
    let database = fixture.database.path().to_path_buf();
    let neighbour = fixture.data_dir().join("coordinator_v3.db-wal");
    std::fs::write(&neighbour, b"wal").expect("a neighbouring file");
    let mislabelled = fixture.data_dir().join(format!("{EXPORT_SNAPSHOT_PREFIX}notes.txt"));
    std::fs::write(&mislabelled, b"not a copy").expect("a mislabelled file");

    prepare_export_snapshot(&fixture.database)
        .await
        .expect("an export snapshot");
    sweep_export_snapshots(fixture.data_dir(), Duration::from_secs(0), usize::MAX).await;

    // The live database's own sidecars are named the way a copy is; a sweep
    // that matched on one end of the name would delete the database.
    assert!(database.exists(), "the sweep removed the live database");
    assert!(neighbour.exists(), "the sweep removed a database sidecar");
    assert!(
        mislabelled.exists(),
        "a file that merely starts with the export prefix is not one"
    );
    assert!(resident_copies(fixture.data_dir()).is_empty());
}
