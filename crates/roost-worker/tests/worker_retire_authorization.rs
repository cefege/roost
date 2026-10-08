#![cfg(unix)]
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
//! MOVE that same call above the keeper probe, and both of those still pass while
//! `the_authorisation_is_spent_after_admission_and_before_the_link` fails: an
//! authorisation spent before the admission that reads it authorises nothing,
//! and both the definition text and the spend report look exactly right.
//!
//! Each test runs its activation in a child process, because the definition is
//! spent through `ProcessEnv` and the process environment is the only channel
//! to it. The three need no serialisation with each other: each has its own
//! scratch root, its own keeper socket, and its own child.
//!
//! A test unwraps the value it is asserting about: a failure there IS the
//! assertion failing. The workspace denies unwrap/expect because a panic on a
//! bad wire value in a running component is a fleet-visible outage, and that
//! reasoning does not reach a test.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "credential_support/scratch.rs"]
mod scratch;

mod retire_support;

use retire_support::{Definition, FakeKeeper, platform, position_of, serve_in_child, spends};
use roost_host::HostPlatform;
use roost_keeper::frames::ChannelBinding;
use roost_worker::runtime::boot::WorkerBoot;
use scratch::Scratch;

/// A keeper that proves it holds one channel, so admission ADOPTS it without
/// the coordinator. These activations have no coordinator, and an EMPTY
/// keeper's replacement waits on the coordinator's open-session set; a keeper
/// held for that read is a boot refusal (`boot_sequence.rs` step 6), just as
/// v2's boot does not complete before `reconcileOpenSessions` has read it.
/// The pid is this process, so the survivor is a live one; no coordinator
/// lists it, so the adoption leaves it alone.
async fn adoptable_keeper(boot: &WorkerBoot, platform: HostPlatform) -> FakeKeeper {
    let held = vec![ChannelBinding {
        channel_id: 1,
        pid: std::process::id(),
    }];
    FakeKeeper::start_holding(boot, platform, held).await
}

/// THE PROPERTY. One activation, one keeper admitted, one authorisation spent:
/// the flag is gone from the service definition afterwards, exactly one spend
/// was reported, and that report says the entry WAS there.
#[tokio::test]
async fn a_force_live_retire_authorisation_is_spent_once_after_admission() {
    let scratch = Scratch::new("retire-spend");
    let platform = platform();
    let definition = Definition::write(scratch.root(), platform, true);
    let boot = retire_support::boot(scratch.root(), platform);
    let _keeper = adoptable_keeper(&boot, platform).await;

    let activation = serve_in_child(scratch.root(), platform, &definition);
    // No coordinator here, so v2's boot refuses at its reconcile
    // (`completeWorkerBootAdmission` throws), after the admission the spend
    // belongs to; a refusal is accepted only from there.
    let outcome = activation.outcome();
    let admitted = position_of(activation.events(), "boot: keeper admitted").is_some();
    assert!(
        outcome.is_ok() || admitted,
        "the worker refused to boot before admitting the keeper: {outcome:?}"
    );

    let spend = spends(activation.events());
    assert_eq!(
        spend.len(),
        1,
        "the authorisation was spent {n} times, and exactly once is the property",
        n = spend.len()
    );
    assert!(spend[0], "the spend did not report the entry it removed");
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
    let scratch = Scratch::new("retire-absent");
    let platform = platform();
    let definition = Definition::write(scratch.root(), platform, false);
    let before = definition.read();
    let boot = retire_support::boot(scratch.root(), platform);
    let _keeper = adoptable_keeper(&boot, platform).await;

    let activation = serve_in_child(scratch.root(), platform, &definition);
    // No coordinator here, so v2's boot refuses at its reconcile
    // (`completeWorkerBootAdmission` throws), after the admission the spend
    // belongs to; a refusal is accepted only from there.
    let outcome = activation.outcome();
    let admitted = position_of(activation.events(), "boot: keeper admitted").is_some();
    assert!(
        outcome.is_ok() || admitted,
        "the worker refused to boot before admitting the keeper: {outcome:?}"
    );

    let spend = spends(activation.events());
    assert_eq!(spend.len(), 1, "the spend was not reported at all");
    assert!(!spend[0], "an absent entry was reported as removed");
    assert_eq!(
        definition.read(),
        before,
        "a definition with nothing to spend was rewritten"
    );
}

/// WHERE the spend lands, which is the half of the property the two tests above
/// cannot see. They both pass with the call moved to the top of `serve_until`,
/// and that move is a regression in both directions at once: an authorisation
/// spent BEFORE keeper admission has been consumed by the admission that read
/// it, so the operator's destructive deploy silently does nothing, and one spent
/// after any keeper work leaves a grant in the unit that re-arms on a keeper
/// that is still holding the PTYs. Neither is visible from the definition text
/// afterwards, which is why this looks at the event ORDER.
///
/// THE MUTATION. Move the `spend_keeper_force_live_retire_authorization` call in
/// `runtime/mod.rs` from after the `KeeperAdmission` step to before the keeper
/// is probed, and this fails on the comparison below.
#[tokio::test]
async fn the_authorisation_is_spent_after_admission_and_before_the_link() {
    let scratch = Scratch::new("retire-order");
    let platform = platform();
    let definition = Definition::write(scratch.root(), platform, true);
    let boot = retire_support::boot(scratch.root(), platform);
    let _keeper = adoptable_keeper(&boot, platform).await;

    let activation = serve_in_child(scratch.root(), platform, &definition);
    // No coordinator here, so v2's boot refuses at its reconcile
    // (`completeWorkerBootAdmission` throws), after the admission the spend
    // belongs to; a refusal is accepted only from there.
    let outcome = activation.outcome();
    let admitted = position_of(activation.events(), "boot: keeper admitted").is_some();
    assert!(
        outcome.is_ok() || admitted,
        "the worker refused to boot before admitting the keeper: {outcome:?}"
    );

    let events = activation.events();
    let admitted = position_of(events, "boot: keeper admitted");
    let spent = events.iter().position(|event| event.removed.is_some());
    let linked = position_of(events, "boot: the coordinator link is starting");
    let admitted = admitted.unwrap_or_else(|| {
        panic!(
            "the activation never reported admitting the keeper, so the spend's position is \
             unprovable: {events:?}"
        )
    });
    let spent = spent.unwrap_or_else(|| {
        panic!("the activation spent nothing, so its position is unprovable: {events:?}")
    });
    assert!(
        admitted < spent,
        "the authorisation was spent BEFORE the keeper was admitted, so the admission that \
         reads it has nothing left to read: {events:?}"
    );
    if let Some(linked) = linked {
        assert!(
            spent < linked,
            "the authorisation was spent AFTER the link started, so a restart between the two \
             re-arms it against a keeper still holding the PTYs: {events:?}"
        );
    }
}
