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
mod update_replace_fixture;

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use roost_cli::update::journal::{PREVIOUS_EXECUTABLE_SUFFIX, SelfUpdateJournal};
use roost_cli::update::rollout::{EXECUTABLE_MODE, ReplaceError, replace_executable};

use update_replace_fixture::{
    Install, NEW, NOW, OLD, VERSION, installed, no_keeper, read, sha256_hex,
};

/// The mode an installed executable carries, as the bytes on disk report it.
fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path)
        .expect("the file has metadata")
        .permissions()
        .mode()
        & 0o7777
}

/// The inode, which a REPLACE must change: writing the target in place would
/// leave every process that had it open reading the old bytes.
fn inode_of(path: &Path) -> u64 {
    std::fs::metadata(path)
        .expect("the file has metadata")
        .ino()
}

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
