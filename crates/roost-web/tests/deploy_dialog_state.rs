//! Pins what the Add Machine dialog may do next, and which answers it is still
//! obliged to hear. Every rule here is a fact a reader or an operator observes:
//!
//! - a local-only install offers neither a Generate action nor a grant, because
//!   a command that cannot work is worse than a refusal;
//! - a declared address no other machine can dial is a CONFIGURATION error, not
//!   a reason to fall back to something else;
//! - "Check again" is a new question, and the previous question's answer is
//!   dropped rather than painted over the new one;
//! - a second Generate mints nothing, and no answer that arrives after the
//!   dialog closed lands on anything.
//!
//! The generation counter is the whole mechanism, so each case drives it through
//! the same public surface the component uses.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_web::components::machines::deploy_state::{
    DeployError, DeployErrorKind, DeployModel, DeployPhase,
};
use roost_web::components::machines::enrollment_origin::EnrollmentDecision;

const DOOR: &str = "https://roost.example.com";
const COMMAND: &str = "curl -fsSL https://example/join.sh | ROOST_COORDINATOR_URL='x' bash";

fn ready() -> EnrollmentDecision {
    EnrollmentDecision::Ready {
        coordinator_url: DOOR.to_owned(),
    }
}

fn local_only() -> EnrollmentDecision {
    EnrollmentDecision::LocalOnly
}

/// A dialog that has been mounted and has landed on a remote door, with the
/// mint already in flight.
fn minting() -> (DeployModel, u64) {
    let mut model = DeployModel::opened();
    let check = model.begin_check();
    assert!(model.accept_identity(check, ready()));
    assert_eq!(model.phase(), DeployPhase::Ready);
    let generation = model.begin_mint().expect("a ready door can mint");
    (model, generation)
}

#[test]
fn a_local_only_install_offers_neither_a_generate_action_nor_a_grant() {
    let mut model = DeployModel::opened();
    let generation = model.begin_check();
    assert!(model.accept_identity(generation, local_only()));
    assert_eq!(model.phase(), DeployPhase::LocalOnly);
    assert!(model.shows_local_access_guide());
    assert_eq!(model.coordinator_url(), None);
    assert_eq!(model.deploy_command(), None);
    assert_eq!(model.begin_mint(), None, "nothing to spend one on");
}

#[test]
fn a_declared_but_undiallable_address_is_a_configuration_error() {
    let mut model = DeployModel::opened();
    let generation = model.begin_check();
    let declared = "https://localhost:8443";
    let unusable = EnrollmentDecision::ConfigurationError {
        declared_url: declared.to_owned(),
    };
    assert!(model.accept_identity(generation, unusable));
    assert_eq!(model.phase(), DeployPhase::Failed);
    let error = model.error().expect("the refusal is shown");
    assert_eq!(error.kind, DeployErrorKind::Configuration);
    assert!(error.detail.contains(declared), "{}", error.detail);
    assert_eq!(model.begin_mint(), None, "a refused door is never used");
}

#[test]
fn a_ready_door_offers_the_generate_action_and_nothing_else() {
    let mut model = DeployModel::opened();
    let generation = model.begin_check();
    assert!(model.accept_identity(generation, ready()));
    assert_eq!(model.phase(), DeployPhase::Ready);
    assert_eq!(model.coordinator_url(), Some(DOOR));
    assert_eq!(model.deploy_command(), None);
    assert!(model.begin_mint().is_some());
}

#[test]
fn pressing_generate_twice_mints_exactly_one_grant() {
    let (mut model, generation) = minting();
    assert_eq!(model.begin_mint(), None, "the second press mints nothing");
    // The identity read inside that mint must not hand the action back.
    assert!(model.accept_identity(generation, ready()));
    assert_eq!(model.phase(), DeployPhase::Minting);
    assert_eq!(model.begin_mint(), None);
    assert!(model.accept_mint(generation, COMMAND.to_owned()));
    assert_eq!(model.phase(), DeployPhase::Generated);
    assert_eq!(model.deploy_command(), Some(COMMAND));
}

#[test]
fn check_again_is_a_new_question_and_the_old_answer_is_dropped() {
    let mut model = DeployModel::opened();
    let first = model.begin_check();
    let second = model.begin_check();
    assert_ne!(first, second, "each check is its own generation");
    assert!(!model.accept_identity(first, ready()));
    assert_eq!(model.coordinator_url(), None);
    assert_eq!(model.phase(), DeployPhase::Checking);
    assert!(model.accept_identity(second, ready()));
    assert_eq!(model.coordinator_url(), Some(DOOR));
}

#[test]
fn a_recheck_forgets_the_command_it_printed_before() {
    let (mut model, generation) = minting();
    assert!(model.accept_mint(generation, COMMAND.to_owned()));
    let recheck = model.begin_check();
    assert_eq!(model.deploy_command(), None);
    assert_eq!(model.phase(), DeployPhase::Checking);
    assert!(model.accept_identity(recheck, local_only()));
    assert_eq!(model.phase(), DeployPhase::LocalOnly);
}

#[test]
fn nothing_in_flight_lands_after_the_dialog_closed() {
    let (mut model, generation) = minting();
    let refused = DeployError::coordinator("the coordinator did not answer");
    model.close();
    assert!(!model.accept_mint(generation, COMMAND.to_owned()));
    assert!(!model.accept_failure(generation, refused));
    assert!(!model.accept_copy(generation, true));
    assert_eq!(model.deploy_command(), None);
    assert!(!model.copied());
}

#[test]
fn a_refusal_inside_a_mint_shows_the_reason_and_publishes_no_command() {
    let (mut model, generation) = minting();
    let refused = DeployError::coordinator("AuthMintBootstrap needs a credential");
    assert!(model.accept_failure(generation, refused));
    assert_eq!(model.phase(), DeployPhase::Failed);
    assert_eq!(model.deploy_command(), None);
    let shown = model.error().expect("the refusal is shown");
    assert_eq!(shown.kind, DeployErrorKind::Coordinator);
}

#[test]
fn a_copy_is_only_claimed_while_the_dialog_that_asked_is_open() {
    let (mut model, generation) = minting();
    assert!(model.accept_mint(generation, COMMAND.to_owned()));
    assert!(model.accept_copy(generation, true));
    assert!(model.copied());
    model.close();
    assert!(!model.accept_copy(generation, true));
}
