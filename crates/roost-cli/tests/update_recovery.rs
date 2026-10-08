#![cfg(unix)]
//! A self-update a previous run left in flight, resolved from its journal before
//! anything else happens, and a journal this build cannot read refused rather
//! than ignored.
//!
//! These are the crash-recovery arms, and they are a separate file from the live
//! replace because a reader looking for "what happens when the machine dies
//! mid-update" should not have to read the happy path first. Every case is a
//! real journal on a real disk, because the decisions here are made by reading
//! the digest at the executable's path and comparing it to what the journal
//! recorded — a fixture that computed its own expected digest would be testing
//! itself.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use roost_cli::services::deploy_journal::sha256_hex;
use roost_cli::update::journal::{KeeperRecord, SELF_UPDATE_JOURNAL_SCHEMA, SelfUpdateJournal};
use roost_cli::update::recovery::{
    RecoveryOutcome, resolve_interrupted_update, roll_back_interrupted_update,
};
use roost_cli::update::rollout::ReplaceError;

struct ServiceDir {
    root: PathBuf,
}

impl ServiceDir {
    fn new(case: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-recover-{}-{case}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&root).expect("the throwaway service dir is created");
        Self { root }
    }

    /// The executable the journal is about, with the bytes it is given.
    fn executable(&self, body: &[u8]) -> PathBuf {
        let path = self.root.join("roost");
        std::fs::write(&path, body).expect("the binary is written");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("the binary is made executable");
        path
    }

    /// A journal in its prepared phase, with a retained copy beside the
    /// executable, exactly as an interrupted swap left them.
    fn prepared_journal(
        &self,
        executable: &std::path::Path,
        source: &[u8],
        target: &[u8],
    ) -> SelfUpdateJournal {
        let journal = SelfUpdateJournal::prepared(
            1_781_900_000_000,
            "v9.9.9",
            executable,
            &sha256_hex(source),
            &sha256_hex(target),
            0o755,
            KeeperRecord::NoRunningKeeper {
                reason: "no coordinator roster on this machine records a keeper here".to_string(),
            },
        )
        .expect("the journal is well formed");
        std::fs::write(journal.previous_path(), source).expect("the retained copy is written");
        journal.write(&self.root).expect("the journal is written");
        journal
    }
}

impl Drop for ServiceDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

const SOURCE: &[u8] = b"the binary this update was replacing\n";
const TARGET: &[u8] = b"the binary this update was installing\n";

/// The crash BEFORE the rename: the executable is still the old one. Nothing
/// was replaced, so there is nothing to undo, but the retained copy was written
/// and leaving it beside a live `roost` is a file nobody can account for.
#[test]
fn a_swap_that_had_not_reached_the_rename_is_cleared_and_its_copy_retired() {
    let service = ServiceDir::new("not-started");
    let executable = service.executable(SOURCE);
    let journal = service.prepared_journal(&executable, SOURCE, TARGET);
    let previous = journal.previous_path();

    let outcome = resolve_interrupted_update(&service.root).expect("recovery is decidable");

    assert_eq!(
        outcome,
        RecoveryOutcome::PreparedCleaned,
        "nothing was replaced, so the update may proceed from a clean machine"
    );
    assert!(!outcome.is_terminal());
    assert_eq!(
        std::fs::read(&executable).expect("the binary is readable"),
        SOURCE,
        "the installed binary was not touched"
    );
    assert!(!previous.exists(), "the retained copy is retired");
    assert!(
        !SelfUpdateJournal::path_in(&service.root).exists(),
        "the journal is cleared"
    );
}

/// The crash AFTER the rename: the executable is already the new binary, so the
/// swap committed and only retiring the retained copy is left.
#[test]
fn a_swap_whose_rename_landed_is_committed_and_its_copy_retired() {
    let service = ServiceDir::new("landed");
    let executable = service.executable(TARGET);
    let journal = service.prepared_journal(&executable, SOURCE, TARGET);
    let previous = journal.previous_path();

    let outcome = resolve_interrupted_update(&service.root).expect("recovery is decidable");

    assert_eq!(
        outcome,
        RecoveryOutcome::InstalledCommitted,
        "the swap landed and is committed"
    );
    assert!(outcome.is_terminal());
    assert_eq!(
        std::fs::read(&executable).expect("the binary is readable"),
        TARGET,
        "the new binary stays installed"
    );
    assert!(!previous.exists(), "the retained copy is retired");
    assert!(
        !SelfUpdateJournal::path_in(&service.root).exists(),
        "the journal is cleared"
    );
}

/// A machine with no journal has nothing in flight. Reading this as "resolved"
/// rather than as an error is what lets a clean machine start.
#[test]
fn a_machine_with_no_journal_has_nothing_in_flight() {
    let service = ServiceDir::new("clean");
    assert_eq!(
        resolve_interrupted_update(&service.root).expect("an absent journal is not a failure"),
        RecoveryOutcome::Nothing
    );
}

/// A binary that hashes to NEITHER recorded digest is not this update's. It is
/// not rolled back over and the journal is not cleared: something else wrote
/// here, and the retained copy is the only other copy of what this update was
/// about.
#[test]
fn a_binary_that_is_neither_copy_this_update_names_is_left_alone() {
    let service = ServiceDir::new("foreign");
    let executable = service.executable(b"a file nobody here wrote\n");
    let journal = service.prepared_journal(&executable, SOURCE, TARGET);

    let failure = resolve_interrupted_update(&service.root)
        .expect_err("an unrecognised binary is not resolved either way");

    assert!(
        matches!(failure, ReplaceError::InstalledBinaryUnrecognised { .. }),
        "the refusal names the three digests: {failure:?}"
    );
    assert_eq!(
        std::fs::read(&executable).expect("the binary is readable"),
        b"a file nobody here wrote\n",
        "the unrecognised file is not overwritten"
    );
    assert!(
        journal.previous_path().exists(),
        "the retained copy is left where it is"
    );
    assert!(
        SelfUpdateJournal::path_in(&service.root).exists(),
        "the journal is kept, because it is the record of what the two digests were"
    );
}

/// A journal whose schema is not this build's is refused. Ignoring it would
/// treat a half-finished swap as if it had never started, and the machine that
/// leaves behind has a binary nobody can account for.
#[test]
fn a_journal_of_an_unknown_schema_is_refused_rather_than_ignored() {
    let service = ServiceDir::new("schema");
    let executable = service.executable(SOURCE);
    let _journal = service.prepared_journal(&executable, SOURCE, TARGET);
    let path = SelfUpdateJournal::path_in(&service.root);
    let mut recorded: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("the journal is readable"))
            .expect("the journal is JSON");
    recorded["schema"] = serde_json::json!(SELF_UPDATE_JOURNAL_SCHEMA + 1);
    std::fs::write(
        &path,
        serde_json::to_vec(&recorded).expect("the doctored journal encodes"),
    )
    .expect("the doctored journal is written");

    let failure = resolve_interrupted_update(&service.root)
        .expect_err("a journal of another schema is not resolvable");

    assert!(
        matches!(failure, ReplaceError::Journal(_)),
        "the refusal is the journal, not a filesystem error: {failure:?}"
    );
    assert!(
        path.exists(),
        "a journal this build cannot read is kept for whoever can"
    );
}

/// An operator who wants the old binary back gets it, and the machine ends in a
/// state the next run can read.
#[test]
fn rolling_back_restores_the_previous_binary_and_clears_the_journal() {
    let service = ServiceDir::new("rollback");
    let executable = service.executable(TARGET);
    let journal = service.prepared_journal(&executable, SOURCE, TARGET);
    let previous = journal.previous_path();

    let outcome = roll_back_interrupted_update(&service.root).expect("the rollback completes");

    assert_eq!(outcome, RecoveryOutcome::PreviousRestored);
    assert!(outcome.rolled_back());
    assert_eq!(
        std::fs::read(&executable).expect("the binary is readable"),
        SOURCE,
        "the previous binary is installed again"
    );
    assert!(!previous.exists(), "the retained copy is retired");
    assert!(
        !SelfUpdateJournal::path_in(&service.root).exists(),
        "the journal is cleared, so the next run starts clean"
    );
}
