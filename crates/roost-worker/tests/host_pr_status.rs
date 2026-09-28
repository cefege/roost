//! Ports v2 `apps/worker/tests/host/pr-status.test.ts` (the rollup reduction and
//! `prStatusEq`) and `tests/host/git-branch.test.ts` (a real `git` inside this
//! repository answers a branch; `/` answers none). The `gh` subprocess itself
//! is driven by `tests/host_folder_facts.rs` with fixture programs.

use roost_protocol::wire::session::{PullRequestChecks, PullRequestState};
use roost_worker::host::PrStatus;
use roost_worker::host::git_branch::GitReader;
use roost_worker::host::pr_status::{RollupEntry, rollup_checks};

fn run(status: &str, conclusion: Option<&str>) -> RollupEntry {
    RollupEntry {
        status: status.to_string(),
        conclusion: conclusion.map(str::to_string),
        state: None,
    }
}

fn commit_status(state: &str) -> RollupEntry {
    RollupEntry {
        state: Some(state.to_string()),
        ..RollupEntry::default()
    }
}

#[test]
fn an_empty_rollup_is_none() {
    assert_eq!(rollup_checks(&[]), PullRequestChecks::None);
}

#[test]
fn all_completed_successes_are_passing() {
    assert_eq!(
        rollup_checks(&[run("COMPLETED", Some("SUCCESS")), commit_status("SUCCESS")]),
        PullRequestChecks::Passing
    );
}

#[test]
fn any_failure_error_or_cancel_is_failing_and_wins_over_pending() {
    assert_eq!(
        rollup_checks(&[run("IN_PROGRESS", None), run("COMPLETED", Some("FAILURE"))]),
        PullRequestChecks::Failing
    );
    assert_eq!(
        rollup_checks(&[commit_status("ERROR")]),
        PullRequestChecks::Failing
    );
    assert_eq!(
        rollup_checks(&[run("COMPLETED", Some("CANCELLED"))]),
        PullRequestChecks::Failing
    );
}

#[test]
fn queued_in_progress_or_pending_without_failure_is_pending() {
    assert_eq!(
        rollup_checks(&[run("QUEUED", None)]),
        PullRequestChecks::Pending
    );
    assert_eq!(
        rollup_checks(&[run("IN_PROGRESS", None)]),
        PullRequestChecks::Pending
    );
    assert_eq!(
        rollup_checks(&[commit_status("PENDING")]),
        PullRequestChecks::Pending
    );
    assert_eq!(
        rollup_checks(&[run("COMPLETED", Some("SUCCESS")), run("IN_PROGRESS", None)]),
        PullRequestChecks::Pending
    );
}

/// v2 reads `conclusion ?? state`: a status that carries a state and no
/// conclusion is judged by its state — including the two failure words only a
/// check run's conclusion normally carries.
#[test]
fn an_entry_without_a_conclusion_is_judged_by_its_state() {
    assert_eq!(
        rollup_checks(&[commit_status("TIMED_OUT")]),
        PullRequestChecks::Failing
    );
    let concluded = RollupEntry {
        status: "COMPLETED".to_string(),
        conclusion: Some("SUCCESS".to_string()),
        state: Some("FAILURE".to_string()),
    };
    assert_eq!(rollup_checks(&[concluded]), PullRequestChecks::Passing);
}

fn base() -> PrStatus {
    PrStatus {
        number: 1,
        state: PullRequestState::Open,
        checks: PullRequestChecks::Passing,
        url: "u".to_string(),
    }
}

#[test]
fn pr_status_equality_is_structural_and_none_equals_only_none() {
    assert_eq!(Some(base()), Some(base()));
    assert_eq!(None::<PrStatus>, None);
    assert_ne!(None, Some(base()));
    let differs = [
        PrStatus {
            checks: PullRequestChecks::Failing,
            ..base()
        },
        PrStatus {
            state: PullRequestState::Merged,
            ..base()
        },
        PrStatus {
            number: 2,
            ..base()
        },
        PrStatus {
            url: "v".to_string(),
            ..base()
        },
    ];
    for other in differs {
        assert_ne!(base(), other);
    }
}

#[test]
fn a_real_git_resolves_a_branch_inside_this_repository_and_none_outside_any() {
    let reader = GitReader::system();
    let here = env!("CARGO_MANIFEST_DIR");
    let branch = reader
        .branch(here)
        .expect("this crate lives inside a git checkout");
    assert!(!branch.is_empty());
    assert_eq!(reader.branch("/"), None);
}
