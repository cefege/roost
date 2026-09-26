//! The keeper force-live retire authorization is SPENT, once, after admission.
//!
//! Why this needs its own test: the value authorises ending every PTY a keeper
//! hosts, and it lives in the worker's service definition. A worker that spends
//! it removes it; a worker that does not runs perfectly well, serves terminals,
//! reconnects to the coordinator, and leaves the next restart armed to destroy
//! all of them. There is no loud failure anywhere in that story — the process
//! that skipped the call is indistinguishable from one that spent it, except in
//! a log line. So the property is asserted here against the real boot.
//!
//! THE MUTATION. Delete the call at `runtime/mod.rs:146` — the
//! `spend_keeper_force_live_retire_authorization` that follows keeper
//! admission — and `a_force_live_retire_authorisation_is_spent_once_after_admission`
//! fails: the definition still carries the flag, and no spend event is emitted.
//!
//! A test unwraps the value it is asserting about: a failure there IS the
//! assertion failing. The workspace denies unwrap/expect because a panic on a
//! bad wire value in a running component is a fleet-visible outage, and that
//! reasoning does not reach a test.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "credential_support/scratch.rs"]
mod scratch;

mod retire_support;

use std::sync::Mutex;

use retire_support::{Definition, FakeKeeper, platform, spend_events};
use scratch::Scratch;

/// Both tests set the same process environment variable, because
/// `runtime::serve_until` spends the authorisation through `ProcessEnv` and
/// has no environment of its own to hand in. Serialised so neither can observe
/// the other's definition.
static ENVIRONMENT: Mutex<()> = Mutex::new(());

/// THE PROPERTY. One activation, one keeper admitted, one authorisation spent:
/// the flag is gone from the service definition afterwards, exactly one spend
/// was reported, and that report says the entry WAS there.
#[tokio::test]
async fn a_force_live_retire_authorisation_is_spent_once_after_admission() {
    let _guard = ENVIRONMENT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let scratch = Scratch::new("retire-spend");
    let platform = platform();
    let definition = Definition::write(scratch.root(), platform, true);
    let boot = retire_support::boot(scratch.root(), platform);
    let _keeper = FakeKeeper::start(&boot, platform).await;
    let (captured, _capture) = spend_events();

    let outcome = retire_support::serve_once(boot).await;
    assert!(outcome.is_ok(), "the worker refused to boot: {outcome:?}");

    let spend = captured();
    assert_eq!(
        spend.len(),
        1,
        "the authorisation was spent {n} times, and exactly once is the property",
        n = spend.len()
    );
    assert_eq!(
        spend[0],
        Some(true),
        "the spend did not report the entry it removed"
    );
    let after = definition.read();
    assert!(
        !after.contains(retire_support::FORCE_LIVE_RETIRE_KEY),
        "the authorisation is still in the service definition and will fire again on the next start"
    );
    assert!(
        after.contains(retire_support::SURVIVOR_KEY),
        "the erase took a neighbouring variable with it: {after}"
    );
}

/// The other half of the report: an activation with nothing to spend says so,
/// and leaves the operator's file byte-for-byte alone. A spend that rewrote a
/// definition it did not change would churn the file and re-run
/// `systemctl --user daemon-reload` for nothing, on every start of every worker.
#[tokio::test]
async fn an_activation_with_no_authorisation_to_spend_reports_that_it_had_none() {
    let _guard = ENVIRONMENT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let scratch = Scratch::new("retire-absent");
    let platform = platform();
    let definition = Definition::write(scratch.root(), platform, false);
    let before = definition.read();
    let boot = retire_support::boot(scratch.root(), platform);
    let _keeper = FakeKeeper::start(&boot, platform).await;
    let (captured, _capture) = spend_events();

    let outcome = retire_support::serve_once(boot).await;
    assert!(outcome.is_ok(), "the worker refused to boot: {outcome:?}");

    let spend = captured();
    assert_eq!(spend.len(), 1, "the spend was not reported at all");
    assert_eq!(
        spend[0],
        Some(false),
        "an absent entry was reported as removed"
    );
    assert_eq!(
        definition.read(),
        before,
        "a definition with nothing to spend was rewritten"
    );
}
