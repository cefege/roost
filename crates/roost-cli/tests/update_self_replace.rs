//! `roost update` and everything it leaves BESIDE the target, plus what it
//! refuses to install at all.
//!
//! Split from `update_self_replace.rs` originally, which held both this and the
//! question of the BYTES the swap installs. Those are different failures and a
//! reader debugging one does not want the other's assertions in the way.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod update_replace_fixture;

use roost_cli::update::journal::{KeeperRecord, PREVIOUS_EXECUTABLE_SUFFIX, SelfUpdateJournal};
use roost_cli::update::rollout::{
    ReplaceError, read_installed, replace_executable, restore_previous,
};

use update_replace_fixture::{
    Install, NEW, NOW, OLD, VERSION, installed, no_keeper, read, sha256_hex,
};

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
