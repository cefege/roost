#![cfg(unix)]
//! Uploads land in the session's project folder: `<folder>/.roost/media`,
//! git-ignored, fixed for the whole operation even if the shell moves, swept
//! by the reaper after the TTL, and never written through a symlinked
//! `.roost`. Pins `attachments::media_dirs` and the media half of
//! `store_paths`, `journal` and `reaper`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod attachment_support;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use attachment_support::{Scratch, digest};
use roost_worker::attachments::file_store::probe_attachment;
use roost_worker::attachments::media_dirs::SessionFolders;
use roost_worker::attachments::reaper::{ATTACHMENT_TTL, sweep_attachments};
use roost_worker::attachments::store_paths::AttachmentBase;
use roost_worker::attachments::system_clock;
use roost_worker::attachments::upload::{AttachmentOperations, DirectChunk, DirectChunkOutcome};

const SESSION: &str = "media-session";

/// A shell whose folder the test moves by hand.
#[derive(Default)]
struct MovableShell {
    folder: Mutex<Option<PathBuf>>,
}

impl MovableShell {
    fn stand_in(&self, folder: &Path) {
        *self.folder.lock().unwrap_or_else(PoisonError::into_inner) = Some(folder.to_path_buf());
    }
}

impl SessionFolders for MovableShell {
    fn session_folder(&self, session_id: &str) -> Option<PathBuf> {
        let folder = self.folder.lock().unwrap_or_else(PoisonError::into_inner);
        (session_id == SESSION).then(|| folder.clone()).flatten()
    }
}

struct Fixture {
    scratch: Scratch,
    shell: Arc<MovableShell>,
    base: AttachmentBase,
    operations: AttachmentOperations,
}

fn fixture(label: &str) -> Fixture {
    let scratch = Scratch::new(label);
    let shell = Arc::new(MovableShell::default());
    let base = scratch
        .base()
        .with_session_folders(Arc::clone(&shell) as Arc<dyn SessionFolders>);
    let operations = AttachmentOperations::new(base.clone(), system_clock());
    Fixture {
        scratch,
        shell,
        base,
        operations,
    }
}

impl Fixture {
    fn project(&self, name: &str) -> PathBuf {
        let project = self.scratch.root.join(name);
        std::fs::create_dir_all(&project).unwrap();
        project
    }

    async fn direct(
        &self,
        upload_id: &str,
        data: &[u8],
        seq: u32,
        offset: u64,
        total: u64,
    ) -> DirectChunkOutcome {
        self.operations
            .accept_direct_chunk(DirectChunk {
                upload_id: upload_id.to_owned(),
                session_id: SESSION.to_owned(),
                filename: "shot.png".to_owned(),
                short_path: false,
                total_bytes: total,
                data: data.to_vec(),
                last: offset + data.len() as u64 == total,
                seq,
                offset,
                chunk_sha256: digest(data),
                carrier_id: "socket-a".to_owned(),
            })
            .await
    }
}

fn committed_path(outcome: DirectChunkOutcome) -> String {
    match outcome {
        DirectChunkOutcome::Committed { abs_path, .. } => abs_path,
        other => panic!("the upload did not commit: {other:?}"),
    }
}

#[tokio::test]
async fn an_upload_lands_git_ignored_in_the_shells_project_folder() {
    let fixture = fixture("media-lands");
    let project = fixture.project("repo");
    fixture.shell.stand_in(&project);

    let saved = committed_path(fixture.direct("one", b"png-bytes", 0, 0, 9).await);

    let media = project.join(".roost").join("media");
    assert_eq!(PathBuf::from(&saved), media.join("shot.png"));
    assert_eq!(std::fs::read(&saved).unwrap(), b"png-bytes");
    let gitignore = std::fs::read_to_string(media.join(".gitignore")).unwrap();
    assert!(gitignore.lines().any(|line| line == "*"), "{gitignore}");
    assert_eq!(fixture.base.media_registry().registered(), vec![media]);
    let probe = probe_attachment(&fixture.base, SESSION, &digest(b"png-bytes"), false);
    assert_eq!((probe.hit, probe.abs_path), (true, saved));
    assert!(
        !fixture.base.session_dir(SESSION).join("shot.png").exists(),
        "nothing lands in the private session directory"
    );
}

#[tokio::test]
async fn a_shell_that_changes_folder_mid_upload_does_not_move_the_operation() {
    let fixture = fixture("media-moves");
    let first = fixture.project("first");
    let second = fixture.project("second");
    fixture.shell.stand_in(&first);

    let progress = fixture.direct("moving", b"abc", 0, 0, 6).await;
    assert!(
        matches!(progress, DirectChunkOutcome::Progress(_)),
        "{progress:?}"
    );
    fixture.shell.stand_in(&second);
    let saved = committed_path(fixture.direct("moving", b"def", 1, 3, 6).await);

    assert_eq!(PathBuf::from(&saved), first.join(".roost/media/shot.png"));
    assert_eq!(std::fs::read(&saved).unwrap(), b"abcdef");
    assert!(!second.join(".roost").exists());
}

#[tokio::test]
async fn a_symlinked_dot_roost_is_never_written_through() {
    let fixture = fixture("media-symlink");
    let project = fixture.project("cloned");
    let elsewhere = fixture.project("elsewhere");
    std::os::unix::fs::symlink(&elsewhere, project.join(".roost")).unwrap();
    fixture.shell.stand_in(&project);

    let saved = committed_path(fixture.direct("linked", b"x", 0, 0, 1).await);

    assert!(PathBuf::from(&saved).starts_with(fixture.base.session_dir(SESSION)));
    assert!(fixture.base.media_registry().registered().is_empty());
}

#[tokio::test]
async fn the_reaper_sweeps_a_registered_project_and_forgets_a_deleted_one() {
    let fixture = fixture("media-reaper");
    let kept = fixture.project("kept");
    let deleted = fixture.project("deleted");
    for project in [&deleted, &kept] {
        fixture.shell.stand_in(project);
        committed_path(
            fixture
                .direct(
                    project.file_name().unwrap().to_str().unwrap(),
                    b"y",
                    0,
                    0,
                    1,
                )
                .await,
        );
    }
    let media = kept.join(".roost/media");
    let stale = SystemTime::now() - (ATTACHMENT_TTL + Duration::from_secs(3600));
    for name in ["old.png", ".gitignore", ".roost-manifest.json"] {
        let path = media.join(name);
        if !path.exists() {
            File::create(&path).unwrap();
        }
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(stale)
            .unwrap();
    }
    std::fs::remove_dir_all(&deleted).unwrap();

    let summary = sweep_attachments(&fixture.base, SystemTime::now()).unwrap();

    assert_eq!(summary.expired, 1);
    assert!(!media.join("old.png").exists());
    assert!(media.join("shot.png").exists(), "a fresh upload survives");
    assert!(media.join(".gitignore").exists() && media.join(".roost-manifest.json").exists());
    assert_eq!(fixture.base.media_registry().registered(), vec![media]);
}
