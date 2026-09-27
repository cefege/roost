//! Which machines `roost push` may converge, and which it defers.
//!
//! Every assertion here is about what an operator or a wrapper script observes:
//! the exit code, the words on the failure line, and which machines a summary
//! calls converged and which it calls deferred. Nothing here reads back a value
//! the code just wrote and compares it with itself.

// A test asserts with `expect`: the message IS the hypothesis about the setup,
// and the workspace lint table denies `expect_used` in every target. The
// production half of this slice contains no `expect` at all.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod push_fixture;

use std::collections::BTreeSet;

use push_fixture::{
    contract, digest, journaled_update, observation, participant, worker, with,
};

use roost_cli::deploy::codes;
use roost_cli::push::plan::{
    FleetRolloutTarget, deferred_fleet_report_lines, fleet_worker_identity_problems,
    partition_fleet_for_rollout, resolve_push_targets, resolve_worker_target, safe_ssh_target,
};
use roost_cli::push::rollout::RolloutAction;
use roost_cli::push::rollout::convergence;

const PRIOR: &str = "1111111111111111111111111111111111111111";
const TARGET: &str = "2222222222222222222222222222222222222222";

fn routable(rows: &[&str]) -> BTreeSet<String> {
    rows.iter()
        .map(|label| digest(&format!("fingerprint-{label}")))
        .collect()
}

#[test]
fn a_name_that_matches_two_machines_is_refused_rather_than_guessed() {
    let first = worker("studio", "shared.example.test", Some(PRIOR), 0);
    let mut second = worker("loft", "shared.example.test", Some(PRIOR), 0);
    second.label = "attic".to_string();
    let roster = vec![first, second];

    let resolved = resolve_worker_target(&roster, "shared.example.test");
    assert!(
        resolved.ambiguous && resolved.worker.is_none(),
        "one address two machines must resolve to neither of them, never to the first row"
    );

    let failure = resolve_push_targets(&roster).expect_err("an ambiguous target is a refusal");
    assert_eq!(failure.code, codes::REJECTED_INVOCATION);
    assert!(
        failure.message.contains("shared.example.test")
            && failure.message.contains("more than one registered worker"),
        "the refusal must name the target and the ambiguity: {}",
        failure.message
    );
}

#[test]
fn a_fingerprint_that_two_rows_claim_is_refused_before_a_single_target_is_chosen() {
    let claimed = digest("fingerprint-studio");
    let first = worker("studio", "studio.example.test", Some(PRIOR), 0);
    let mut second = worker("loft", "loft.example.test", Some(PRIOR), 0);
    second.fingerprint = claimed.clone();
    let roster = vec![first, second];

    let resolved = resolve_worker_target(&roster, &claimed);
    assert!(
        resolved.ambiguous && resolved.worker.is_none(),
        "a fingerprint two rows claim resolves to neither row"
    );

    let problems = fleet_worker_identity_problems(&roster);
    assert_eq!(problems.len(), 1, "exactly one identity defect: {problems:?}");
    assert!(
        problems[0].contains(&claimed) && problems[0].contains("duplicate"),
        "the operator must be shown which identity is claimed twice: {}",
        problems[0]
    );
}

#[test]
fn a_registry_fingerprint_that_is_not_a_full_identity_refuses_the_whole_push() {
    let mut short = worker("studio", "studio.example.test", Some(PRIOR), 0);
    short.fingerprint = "abc123".to_string();
    let mut shouted = worker("loft", "loft.example.test", Some(PRIOR), 0);
    shouted.fingerprint = "A".repeat(64);

    let problems = fleet_worker_identity_problems(&[short, shouted]);
    assert_eq!(problems.len(), 2, "both malformed rows: {problems:?}");
    assert!(
        problems.iter().any(|line| line.contains("invalid worker fingerprint")),
        "a fingerprint that is not 64 lowercase hex is not an identity: {problems:?}"
    );
}

#[test]
fn an_address_ssh_could_be_handed_is_refused_instead_of_quoted_and_hoped_for() {
    for hostile in [
        "studio;rm -rf /",
        "studio host",
        "studio/../etc",
        "-oProxyCommand=id",
        "",
        "  ",
    ] {
        assert!(
            safe_ssh_target(hostile).is_none(),
            "{hostile:?} must never reach an ssh argv"
        );
    }
    assert_eq!(
        safe_ssh_target(" studio.example.test. ").as_deref(),
        Some("studio.example.test."),
        "a well-formed FQDN with a trailing root dot is addressable"
    );
    assert_eq!(
        safe_ssh_target("deploy@studio-1.local:2222").as_deref(),
        Some("deploy@studio-1.local:2222"),
        "a user, a dash and a port are all ssh's own syntax"
    );
}

#[test]
fn a_machine_whose_address_is_not_a_safe_ssh_target_stops_the_push_before_anything() {
    let hostile = worker("studio", "studio;rm -rf /", Some(PRIOR), 0);
    let failure = resolve_push_targets(&[hostile]).expect_err("an unsafe address is a refusal");
    assert_eq!(failure.code, codes::REJECTED_INVOCATION);
    assert!(
        failure.message.contains("invalid ssh deployment target"),
        "the refusal must name the reason: {}",
        failure.message
    );
}

#[test]
fn a_push_with_no_registered_worker_refuses_rather_than_reporting_an_empty_success() {
    let failure = resolve_push_targets(&[]).expect_err("an empty fleet is a refusal");
    assert_eq!(failure.code, codes::REJECTED_INVOCATION);
    assert!(
        failure.message.contains("at least one registered worker"),
        "{}",
        failure.message
    );
}

#[test]
fn a_machine_that_cannot_be_converged_now_is_deferred_and_not_counted_as_a_participant() {
    let candidates = ["studio", "loft", "shed"]
        .iter()
        .map(|label| FleetRolloutTarget {
            fingerprint: digest(&format!("fingerprint-{label}")),
            host: format!("{label}.example.test"),
        })
        .collect::<Vec<_>>();
    let third = "3333333333333333333333333333333333333333";
    let roster = vec![
        worker("studio", "studio.example.test", Some(PRIOR), 0),
        with(worker("loft", "loft.example.test", Some(PRIOR), 0), Some(PRIOR), true),
        with(worker("shed", "shed.example.test", Some(PRIOR), 0), Some(third), false),
    ];

    let partition =
        partition_fleet_for_rollout(&candidates, &roster, &routable(&["studio"]), PRIOR);

    assert_eq!(
        partition.participants.len(),
        1,
        "only the machine that is fresh, reachable and on the prior commit is a participant"
    );
    assert_eq!(partition.participants[0].host, "studio.example.test");
    assert_eq!(
        partition.deferred.len(),
        2,
        "a sleeping machine and a drifted one are both deferred, not failed: {:?}",
        partition.deferred
    );
    let reasons: BTreeSet<&str> = partition
        .deferred
        .iter()
        .map(|machine| machine.reason.as_str())
        .collect();
    assert!(reasons.contains("stale"), "{reasons:?}");
    assert!(
        reasons.contains("not reachable"),
        "a machine this box could not dial is deferred as unreachable: {reasons:?}"
    );

    let lines = deferred_fleet_report_lines(&partition.deferred);
    let report = lines.join("\n");
    assert!(report.contains("2 machines deferred — update pending:"), "{report}");
    assert!(report.contains("loft: stale"), "{report}");
    assert!(report.contains("roost deploy <host>"), "{report}");
}

#[test]
fn a_deferred_machine_is_reported_beside_a_success_and_is_never_counted_as_converged() {
    let deferred = partition_fleet_for_rollout(
        &[FleetRolloutTarget {
            fingerprint: digest("fingerprint-loft"),
            host: "loft.example.test".to_string(),
        }],
        &[with(
            worker("loft", "loft.example.test", Some(PRIOR), 0),
            Some(PRIOR),
            true,
        )],
        &BTreeSet::new(),
        PRIOR,
    );
    assert!(deferred.participants.is_empty());

    let report = deferred_fleet_report_lines(&deferred.deferred).join("\n");
    assert!(
        report.contains("update pending") && !report.contains(TARGET),
        "the summary says the machine is pending and never claims it carries the commit: {report}"
    );

    assert!(
        deferred_fleet_report_lines(&[]).is_empty(),
        "a fleet with nothing deferred prints no summary at all"
    );
}

#[test]
fn a_rollback_that_finds_a_machine_on_a_third_commit_names_it() {
    let third = "4444444444444444444444444444444444444444";
    let row = with(
        worker("studio", "studio.example.test", Some(PRIOR), 0),
        Some(third),
        false,
    );
    let problems = convergence::fleet_convergence_problems(
        std::slice::from_ref(&row),
        &[],
        PRIOR,
        RolloutAction::Rollback,
        None,
        Some(TARGET),
    );
    assert!(
        problems
            .iter()
            .any(|line| line.contains("outside this rollout")),
        "a machine on a third commit is evidence this rollout dragged a worker it never \
         journalled: {problems:?}"
    );
}

#[test]
fn a_deferred_machine_reporting_either_end_of_the_rollout_is_not_a_finding() {
    let update = journaled_update(
        &observation("keeper-a", 0),
        &contract("keeper-a"),
        &BTreeSet::new(),
    );
    let journalled = vec![participant("studio", "studio.example.test", update)];
    let on_prior = worker("studio", "studio.example.test", Some(PRIOR), 0);
    let deferred_on_prior = with(
        worker("loft", "loft.example.test", Some(PRIOR), 0),
        Some(PRIOR),
        false,
    );
    let deferred_on_target = with(
        worker("shed", "shed.example.test", Some(TARGET), 0),
        Some(TARGET),
        false,
    );
    let never_reported = with(worker("attic", "attic.example.test", None, 0), None, false);
    let roster = vec![on_prior, deferred_on_prior, deferred_on_target, never_reported];

    let problems = convergence::fleet_convergence_problems(
        &roster,
        &journalled,
        PRIOR,
        RolloutAction::Rollback,
        None,
        Some(TARGET),
    );
    assert!(
        problems.is_empty(),
        "a deferred machine keeps its own commit, and one that never reported a SHA is \
         unproven rather than mutated: {problems:?}"
    );
}
