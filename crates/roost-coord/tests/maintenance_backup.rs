//! The nightly backup's promises: a verified archive published by rename, a
//! fixed keep count, and permissions that keep another user out.
//!
//! The property that matters most is the first one. An archive that is written
//! in place is an archive a reader can open halfway through, and a failed
//! write in place is an archive that was there before the failure -- every
//! previous night, up to fourteen of them -- replaced by a truncated file that
//! still has a name and a plausible size.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use async_compression::tokio::bufread::GzipDecoder;
use tokio::io::AsyncReadExt;

use roost_coord::db::CoordDb;
use roost_coord::maintenance::backup::{
    BackupReason, MAX_BACKUPS, backup_tag, backups_dir, list_archives, run_backup_at,
};
use roost_coord::maintenance::gzip_file::GZIP_FILE_CHUNK_BYTES;

/// 2024-02-29T12:34:56.789Z. A leap day, so a wrong civil-date conversion
/// cannot pass by luck.
const FIXED_MS: i64 = 1_709_210_096_789;

struct BackupFixture {
    database: CoordDb,
    root: PathBuf,
}

impl BackupFixture {
    async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!("roost-backup-{label}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database = roost_coord::db::open(&root.join("coord.db"))
            .await
            .expect("a migrated database");
        Self { database, root }
    }

    fn directory(&self) -> PathBuf {
        backups_dir(self.database.path())
    }

    /// Create a directory holding a plausible archive name, as if a previous
    /// night had run.
    async fn seed_archive(&self, tag: &str) -> PathBuf {
        let path = self.directory().join(format!("coord_v2.{tag}.db.gz"));
        tokio::fs::create_dir_all(self.directory())
            .await
            .expect("a backups dir");
        tokio::fs::write(&path, format!("previous archive {tag}"))
            .await
            .expect("a seeded archive");
        path
    }

    /// A name shaped like an archive's staging file, which is what a failed
    /// write has to collide with.
    fn staging_path(&self, tag: &str) -> PathBuf {
        self.directory().join(format!(".coord_v2.{tag}.db.gz.tmp"))
    }
}

impl Drop for BackupFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path)
        .expect("the path exists")
        .permissions()
        .mode()
        & 0o777
}

async fn read_bytes(path: &Path) -> Vec<u8> {
    tokio::fs::read(path).await.expect("the file is readable")
}

// ── what a backup produces ─────────────────────────────────────────────────

#[tokio::test]
async fn an_archive_is_a_gzip_of_the_database_under_a_name_that_sorts_by_time() {
    let fixture = BackupFixture::new("archive").await;

    let record = run_backup_at(&fixture.database, BackupReason::Scheduled, FIXED_MS)
        .await
        .expect("a backup");

    assert_eq!(
        record.path,
        fixture
            .directory()
            .join("coord_v2.2024-02-29T12-34-56-789.db.gz"),
        "the name is the UTC instant with the filename-unsafe characters replaced"
    );
    let raw = read_bytes(&record.path).await;
    assert_eq!(&raw[..2], &[0x1f, 0x8b], "the archive is a gzip member");

    let mut decompressed = Vec::new();
    GzipDecoder::new(&raw[..])
        .read_to_end(&mut decompressed)
        .await
        .expect("the archive decompresses");
    assert_eq!(
        &decompressed[..16],
        b"SQLite format 3\0",
        "what was compressed is a database, not a truncated stream"
    );
    assert!(
        record.bytes_in > 0 && record.bytes_out > 0,
        "both byte counts are reported: {:?}",
        (record.bytes_in, record.bytes_out)
    );
    assert_eq!(record.bytes_in, decompressed.len() as u64);
}

#[tokio::test]
async fn the_uncompressed_snapshot_never_outlives_the_backup() {
    let fixture = BackupFixture::new("snapshot-gone").await;

    run_backup_at(&fixture.database, BackupReason::Scheduled, FIXED_MS)
        .await
        .expect("a backup");

    let leftovers: Vec<String> = std::fs::read_dir(fixture.directory())
        .expect("the backups dir")
        .map(|entry| {
            entry
                .expect("a dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.contains("snapshot") || name.ends_with(".tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "a snapshot of a whole database must not be left beside the archive: {leftovers:?}"
    );
}

#[tokio::test]
async fn the_backups_directory_is_owner_only_and_so_is_the_archive() {
    let fixture = BackupFixture::new("modes").await;

    let record = run_backup_at(&fixture.database, BackupReason::Scheduled, FIXED_MS)
        .await
        .expect("a backup");

    assert_eq!(
        mode_of(&fixture.directory()),
        0o700,
        "another user must not be able to list the backups"
    );
    assert_eq!(
        mode_of(&record.path),
        0o600,
        "another user must not be able to read a database"
    );
}

// ── a failed write must not cost the last good archive ─────────────────────

#[tokio::test]
async fn a_write_that_cannot_start_leaves_the_previous_archive_byte_for_byte() {
    let fixture = BackupFixture::new("clobber-open").await;
    let published = run_backup_at(&fixture.database, BackupReason::Scheduled, FIXED_MS)
        .await
        .expect("the first backup");
    let before = read_bytes(&published.path).await;

    // The staging path cannot be opened for writing, so the archive is never
    // written anywhere.
    tokio::fs::create_dir_all(fixture.staging_path(&backup_tag(FIXED_MS)))
        .await
        .expect("a staging path that is a directory");

    let outcome = run_backup_at(&fixture.database, BackupReason::Scheduled, FIXED_MS).await;
    assert!(
        outcome.is_err(),
        "the backup published {} even though its staging path could not be written, so \
         the previous archive under that name has already been overwritten",
        published.path.display()
    );
    let failure = outcome.expect_err("checked above");
    assert!(
        failure.to_string().contains("i/o"),
        "the failure names the write: {failure}"
    );
    assert_eq!(
        read_bytes(&published.path).await,
        before,
        "the previous archive was lost: a write that fails must leave it intact"
    );
    let archives = list_archives(&fixture.directory()).await;
    assert_eq!(archives, vec![published.path.clone()]);
}

#[tokio::test]
async fn a_write_that_fails_mid_stream_leaves_the_previous_archive_byte_for_byte() {
    let fixture = BackupFixture::new("clobber-full").await;
    let published = run_backup_at(&fixture.database, BackupReason::Scheduled, FIXED_MS)
        .await
        .expect("the first backup");
    let before = read_bytes(&published.path).await;

    // A staging path that opens but never accepts a byte, so the failure lands
    // with the archive already half written rather than before it starts.
    std::os::unix::fs::symlink("/dev/full", fixture.staging_path(&backup_tag(FIXED_MS)))
        .expect("a staging path that cannot be written");

    let outcome = run_backup_at(&fixture.database, BackupReason::PreMigration, FIXED_MS).await;
    assert!(
        outcome.is_err(),
        "the backup published {} even though its staging path could not be written, so \
         the previous archive under that name has already been overwritten",
        published.path.display()
    );
    let failure = outcome.expect_err("checked above");
    assert!(
        failure.to_string().contains("i/o"),
        "the failure names the write: {failure}"
    );

    assert_eq!(
        read_bytes(&published.path).await,
        before,
        "the previous archive was lost: a partial write must never reach the real name"
    );
    assert!(
        !fixture.staging_path(&backup_tag(FIXED_MS)).exists(),
        "the staging file is cleaned up even when the write failed"
    );
}

#[tokio::test]
async fn a_failed_first_backup_publishes_nothing_at_all() {
    let fixture = BackupFixture::new("first-fails").await;
    tokio::fs::create_dir_all(fixture.staging_path(&backup_tag(FIXED_MS)))
        .await
        .expect("a staging path that is a directory");

    let outcome = run_backup_at(&fixture.database, BackupReason::Scheduled, FIXED_MS).await;
    assert!(
        outcome.is_err(),
        "a backup that could not stage its archive published one anyway"
    );
    assert!(
        list_archives(&fixture.directory()).await.is_empty(),
        "a backup that failed has published no archive under any name"
    );
}

// ── the keep count ─────────────────────────────────────────────────────────

#[tokio::test]
async fn the_prune_keeps_exactly_the_keep_count_and_drops_the_oldest() {
    let fixture = BackupFixture::new("prune").await;
    for night in 0..=MAX_BACKUPS as i64 {
        fixture
            .seed_archive(&backup_tag(FIXED_MS - (night + 1) * 86_400_000))
            .await;
    }
    let oldest = backup_tag(FIXED_MS - (MAX_BACKUPS as i64 + 1) * 86_400_000);

    run_backup_at(&fixture.database, BackupReason::Scheduled, FIXED_MS)
        .await
        .expect("a backup");

    let archives = list_archives(&fixture.directory()).await;
    assert_eq!(
        archives.len(),
        MAX_BACKUPS,
        "the keep count is a keep count, not a floor: {:?}",
        archives
    );
    assert!(
        !archives
            .iter()
            .any(|path| path.ends_with(&format!("coord_v2.{oldest}.db.gz"))),
        "the oldest archive is the one that goes"
    );
    assert!(
        archives
            .iter()
            .any(|path| path.ends_with("coord_v2.2024-02-29T12-34-56-789.db.gz")),
        "the archive just written is among the kept"
    );
}

#[tokio::test]
async fn a_prune_that_cannot_finish_does_not_fail_the_backup() {
    let fixture = BackupFixture::new("prune-fails").await;
    for night in 0..MAX_BACKUPS as i64 {
        fixture
            .seed_archive(&backup_tag(FIXED_MS - (night + 1) * 86_400_000))
            .await;
    }
    // The oldest entry is a directory, so removing it fails. v2 catches that
    // per entry and keeps going (`backup.ts:78-84`); the consequence that
    // matters is the one outside the loop: the backup is still a success.
    let unremovable = fixture.directory().join(format!(
        "coord_v2.{}.db.gz",
        backup_tag(FIXED_MS - 90 * 86_400_000)
    ));
    tokio::fs::create_dir_all(&unremovable)
        .await
        .expect("an unremovable entry");
    tokio::fs::write(unremovable.join("occupied"), b"x")
        .await
        .expect("a non-empty directory");

    let record = run_backup_at(&fixture.database, BackupReason::Scheduled, FIXED_MS)
        .await
        .expect("the backup succeeds even though the prune cannot");

    assert!(record.path.exists(), "the new archive is on disk");
    let archives = list_archives(&fixture.directory()).await;
    assert_eq!(
        archives.len(),
        MAX_BACKUPS + 1,
        "only the entries that could be removed were: {:?}",
        archives
    );
    assert!(
        archives.contains(&unremovable),
        "the failed entry is left alone"
    );
}

// ── the name ───────────────────────────────────────────────────────────────

#[test]
fn an_archive_tag_is_utc_sorted_text_and_a_whole_millisecond_earlier() {
    assert_eq!(backup_tag(0), "1970-01-01T00-00-00-000");
    assert_eq!(backup_tag(1), "1970-01-01T00-00-00-001");
    assert_eq!(backup_tag(FIXED_MS), "2024-02-29T12-34-56-789");
    assert_eq!(
        backup_tag(-14_182_940_000),
        "1969-07-20T20-17-40-000",
        "an instant before the epoch is a date, not a negative number"
    );
    assert_eq!(
        backup_tag(1_709_210_096_789 + 1),
        "2024-02-29T12-34-56-790",
        "the millisecond field carries, so two backups in one second are distinct names"
    );
    assert!(
        backup_tag(999) < backup_tag(1000),
        "the prune orders by name, so name order is time order"
    );
}

#[test]
fn the_compression_slice_is_the_one_the_backup_bounds_memory_with() {
    assert_eq!(GZIP_FILE_CHUNK_BYTES, 1024 * 1024);
}
