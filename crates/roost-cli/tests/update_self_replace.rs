//! What `roost update` does to the file on disk, exercised against a real one.
//!
//! The property under test is the one a reader cannot get from the code: a
//! self-replace that fails leaves the INSTALLED BINARY byte-identical and a
//! journal naming where the previous copy is, and a self-replace that succeeds
//! leaves nothing beside the target but the new binary. Both are claims about
//! bytes and inodes on a filesystem, so every case here drives the real
//! `replace_executable` against a real directory and then reads the directory
//! back.
//!
//! The negative cases matter more than the positive one. A function that
//! returned `Ok` and had written the target with `fs::write` passes a test that
//! only checks the happy path, and the machine that finds out is one whose
//! `roost` was truncated by a power cut. So each failure case asserts the bytes
//! that were there are still there.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use roost_cli::services::deploy_journal::sha256_hex;
use roost_cli::update::candidate::VerifiedCandidate;
use roost_cli::update::journal::{KeeperRecord, PREVIOUS_EXECUTABLE_SUFFIX, SelfUpdateJournal};
use roost_cli::update::rollout::{
    EXECUTABLE_MODE, InstalledBinary, ReplaceError, read_installed, replace_executable,
    restore_previous,
};

/// A throwaway install directory, removed when the test ends.
struct Install {
    root: PathBuf,
}

impl Install {
    fn new(case: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-update-{}-{case}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(root.join("service")).expect("the throwaway tree is created");
        Self { root }
    }

    /// The install's `roost`, with the bytes and mode an installer would leave.
    fn install_binary(&self, body: &[u8], mode: u32) -> PathBuf {
        let path = self.root.join("roost");
        std::fs::write(&path, body).expect("the fake binary is written");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
            .expect("the fake binary is made executable");
        path
    }

    /// A verified candidate beside the installed binary, as the downloader
    /// leaves it after the digest matched the release's sidecar.
    fn candidate(&self, body: &[u8]) -> VerifiedCandidate {
        let path = self.root.join("roost.candidate");
        std::fs::write(&path, body).expect("the candidate is written");
        VerifiedCandidate {
            path,
            sha256: sha256_hex(body),
            url: "https://example.invalid/roost-linux-x64".to_string(),
        }
    }

    fn service_dir(&self) -> PathBuf {
        self.root.join("service")
    }

    /// Every name in the install directory, so a test asserts what was left
    /// behind as well as what the target now holds.
    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.root)
            .expect("the install directory is readable")
            .map(|entry| {
                entry
                    .expect("a directory entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }
}

impl Drop for Install {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|error| panic!("{} is readable: {error}", path.display()))
}

fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path)
        .expect("the file has metadata")
        .permissions()
        .mode()
        & 0o7777
}

fn inode_of(path: &Path) -> u64 {
    std::fs::metadata(path)
        .expect("the file has metadata")
        .ino()
}

fn no_keeper() -> Option<KeeperRecord> {
    Some(KeeperRecord::NoRunningKeeper {
        reason: "no coordinator roster on this machine records a keeper here".to_string(),
    })
}

fn installed(body: &[u8], mode: u32, keeper: Option<KeeperRecord>) -> InstalledBinary {
    InstalledBinary {
        sha256: sha256_hex(body),
        mode,
        keeper,
    }
}

const OLD: &[u8] = b"#!/bin/sh\n# the installed roost\necho old\n";
const NEW: &[u8] = b"#!/bin/sh\n# the published roost\necho new\nand more of it\n";
const NOW: i64 = 1_781_900_000_000;
const VERSION: &str = "v9.9.9";

/// The floor: a successful replace installs the candidate's bytes and the mode an
/// installed executable carries.
#[test]
fn a_successful_replace_installs_the_published_bytes_and_the_executable_mode() {
    let install = Install::new("success");
    let executable = install.install_binary(OLD, 0o755);
    let candidate = install.candidate(NEW);

    let outcome = replace_executable(
        &executable,
        &candidate,
        VERSION,
        &installed(OLD, 0o755, no_keeper()),
        &install.service_dir(),
        NOW,
    )
    .expect("the replace settles");

    assert_eq!(
        read(&executable),
        NEW,
        "the installed binary is the published one"
    );
    assert_eq!(
        mode_of(&executable),
        EXECUTABLE_MODE,
        "the installed binary is executable"
    );
    assert_eq!(outcome.target_version, VERSION);
}

/// The distinction the whole command exists for. A replace implemented as
/// `fs::write` passes the case above and cannot pass this one, because the
/// inode a running `roost` was mapped from is still the old one, still holding
/// the old bytes. This is also the property that makes the operation safe on a
/// machine that is running the binary being replaced.
#[test]
fn a_replace_leaves_the_running_image_reading_the_bytes_it_started_with() {
    use std::io::Read;

    let install = Install::new("running-image");
    let executable = install.install_binary(OLD, 0o755);
    let candidate = install.candidate(NEW);
    // Opened before the swap and still open across it: this stands in for a
    // process executing from the file, which is what the kernel holds the old
    // inode alive for.
    let mut running = std::fs::File::open(&executable).expect("the running image is opened");
    let inode_before = inode_of(&executable);

    replace_executable(
        &executable,
        &candidate,
        VERSION,
        &installed(OLD, 0o755, no_keeper()),
        &install.service_dir(),
        NOW,
    )
    .expect("the replace settles");

    assert_ne!(
        inode_of(&executable),
        inode_before,
        "the swap is a rename, so the target is a new file rather than a truncated one"
    );
    let mut held = Vec::new();
    running
        .read_to_end(&mut held)
        .expect("the held image is readable");
    assert_eq!(
        held, OLD,
        "the running image still reads the bytes it began with"
    );
}

/// A swap that cannot complete leaves the installed binary byte-identical, its
/// mode untouched, and a journal naming where the previous copy is.
///
/// The fault is a DIRECTORY sitting where the retained copy belongs. It is used
/// rather than a permission bit because a permission bit does not stop a
/// privileged test runner, and a fault that only fires for one user is a fault
/// this test would silently stop exercising.
#[test]
fn a_swap_that_fails_leaves_the_installed_binary_byte_identical_and_a_journal() {
    let install = Install::new("fails");
    let executable = install.install_binary(OLD, 0o700);
    let candidate = install.candidate(NEW);
    let previous = install
        .root
        .join(format!("roost{PREVIOUS_EXECUTABLE_SUFFIX}"));
    std::fs::create_dir(&previous).expect("the rollback path is occupied by a directory");

    let failure = replace_executable(
        &executable,
        &candidate,
        VERSION,
        &installed(OLD, 0o700, no_keeper()),
        &install.service_dir(),
        NOW,
    )
    .expect_err("the swap cannot complete");

    assert!(
        matches!(failure, ReplaceError::PreviousNotRetained { .. }),
        "the failure names the retain, which is the step that failed: {failure:?}"
    );
    assert_eq!(
        read(&executable),
        OLD,
        "the installed binary is byte-identical"
    );
    assert_eq!(
        mode_of(&executable),
        0o700,
        "the installed mode is untouched"
    );

    let journal = SelfUpdateJournal::load(&install.service_dir())
        .expect("the journal parses")
        .expect("a journal was left behind naming where the previous copy is");
    assert_eq!(
        journal.previous_path(),
        previous,
        "the journal names the previous copy's own path"
    );
    assert_eq!(
        journal.source_binary_sha256,
        sha256_hex(OLD),
        "the journal records the digest the swap would have replaced"
    );
    assert_eq!(
        journal.target_binary_sha256,
        sha256_hex(NEW),
        "the journal records the digest the swap would have installed"
    );
}

/// A successful replace leaves no temporary file beside the target. A candidate
/// that outlives the swap is a file an operator cannot account for and one a
#[test]
fn a_successful_replace_leaves_no_temporary_file_beside_the_target() {
    let install = Install::new("no-temp");
    let executable = install.install_binary(OLD, 0o755);
    let candidate = install.candidate(NEW);

    replace_executable(
        &executable,
        &candidate,
        VERSION,
        &installed(OLD, 0o755, no_keeper()),
        &install.service_dir(),
        NOW,
    )
    .expect("the replace settles");

    let entries = install.entries();
    assert_eq!(
        entries,
        vec!["roost".to_string(), "service".to_string()],
        "the install directory holds the binary and the service directory the fixture made, \
         and nothing else"
    );
    assert!(
        entries.iter().all(|entry| !entry.contains("candidate")),
        "the staged candidate is gone after the swap: {entries:?}"
    );
    assert!(
        !entries
            .iter()
            .any(|entry| entry == &format!("roost.{PREVIOUS_EXECUTABLE_SUFFIX}")),
        "the retained previous copy is renamed over, not left beside the target: {entries:?}"
    );
    assert!(
        !SelfUpdateJournal::path_in(&install.service_dir()).exists(),
        "the journal is retired once the new binary is proved"
    );
}

/// A candidate whose bytes are not what the release published is refused, and
/// nothing on the machine is touched: not the installed binary, not a retained
/// copy, and not a journal.
#[test]
fn a_candidate_that_is_not_what_the_release_published_is_refused() {
    let install = Install::new("digest");
    let executable = install.install_binary(OLD, 0o755);
    let candidate = install.candidate(NEW);
    // Somebody writes through the staged file after it was verified.
    std::fs::write(&candidate.path, b"something else entirely\n").expect("the bytes change");

    let failure = replace_executable(
        &executable,
        &candidate,
        VERSION,
        &installed(OLD, 0o755, no_keeper()),
        &install.service_dir(),
        NOW,
    )
    .expect_err("a candidate that does not match its published digest is refused");

    assert!(
        matches!(failure, ReplaceError::CandidateChanged { .. }),
        "the refusal names the digest, not the filesystem: {failure:?}"
    );
    assert_eq!(read(&executable), OLD, "the installed binary is untouched");
    assert!(
        !SelfUpdateJournal::path_in(&install.service_dir()).exists(),
        "a refused candidate writes no journal"
    );
}

/// A swap reached with no keeper decision recorded is refused. The check is the
/// point of the gate, and reaching the swap without one is the case where a
/// keeper's PTYs would be risked with nothing having decided it.
#[test]
fn a_swap_with_no_keeper_decision_recorded_is_refused() {
    let install = Install::new("no-gate");
    let executable = install.install_binary(OLD, 0o755);
    let candidate = install.candidate(NEW);
    let undecided = read_installed(&executable).expect("the installed binary is readable");
    assert!(
        undecided.keeper.is_none(),
        "a binary read from disk carries no keeper decision, which is the hazard"
    );

    let failure = replace_executable(
        &executable,
        &candidate,
        VERSION,
        &undecided,
        &install.service_dir(),
        NOW,
    )
    .expect_err("a swap with no keeper decision is refused");

    assert!(
        matches!(failure, ReplaceError::KeeperNotAdmissible { .. }),
        "the refusal is the keeper gate, not a filesystem error: {failure:?}"
    );
    assert_eq!(read(&executable), OLD, "the installed binary is untouched");
}

/// A retained copy whose digest is not the one the journal recorded is never
/// installed back. A truncated save restored over a working binary is worse than
/// no rollback at all, so this is a refusal and not a best effort.
#[test]
fn a_truncated_retained_copy_is_never_installed_back() {
    let install = Install::new("truncated");
    let executable = install.install_binary(OLD, 0o755);
    let service_dir = install.service_dir();
    let journal = SelfUpdateJournal::prepared(
        NOW,
        VERSION,
        &executable,
        &sha256_hex(OLD),
        &sha256_hex(NEW),
        0o755,
        KeeperRecord::NoRunningKeeper {
            reason: "no coordinator roster on this machine records a keeper here".to_string(),
        },
    )
    .expect("the journal is well formed");
    std::fs::write(journal.previous_path(), b"truncated").expect("the retained copy is damaged");
    let installed_before = read(&executable);

    let failure = restore_previous(&journal).expect_err("a truncated copy is refused");

    assert!(
        matches!(failure, ReplaceError::PreviousCopyTruncated { .. }),
        "the refusal names the truncated copy: {failure:?}"
    );
    assert_eq!(
        read(&executable),
        installed_before,
        "nothing is written over a binary the restore could not prove"
    );
}
