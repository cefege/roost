//! The boot sequence's ORDER, and the readiness claim it ends in. The
//! configuration refusals that happen before anything is started are
//! `worker_boot_config.rs`.
//!
//! Why the slices' own tests cannot cover this: `boot_keeper`, `link_barrier`,
//! `outbox` and `backoff` each test their own decision in isolation, and each of
//! them passes no matter what order the worker calls them in. An order bug —
//! probing a keeper before the identity is settled, announcing readiness before
//! reconciling — is invisible to all of them, and it is the class that ends a
//! user's terminals rather than the class that returns a wrong number.

// A test unwraps the value it is asserting about: a failure there IS the
// assertion failing, which is what a test wants. The workspace denies
// unwrap/expect because a panic on a bad wire value in a running component is a
// fleet-visible outage, and that reasoning does not reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_worker::runtime::boot_order::{
    BOOT_ORDER, BootSequence, OutOfOrderStep, Readiness, ReadyStep, StepId,
};

/// The order is written down once, and every step in it says what moving it
/// would break. A step with an empty reason is a step nobody can defend later.
///
/// THIS VECTOR MOVED, DELIBERATELY, and that is the shape of the change rather
/// than a wrinkle in it. It is the oracle this file declares at :71-73, so a
/// reorder of the declared order is supposed to move it — and the reason it
/// moved is that the DECLARATION was wrong about the executed order, not that
/// the execution drifted. `boot_sequence::run` records `KeeperAdmission`
/// before `CoordinatorLink`, always has, and the stop-time log printed that
/// sequence while this file asserted the opposite.
///
/// The rationale inside each `because` is a separate matter and is NOT fixed
/// here: `coordinator-link`'s still claims the link DIALS before the keeper is
/// admitted, which is false — the dial is `link.run`, the last statement of
/// the function. What is true is that the open-session set the decision needs
/// is read over Connect at step 5, before the keeper, and the link is only
/// BUILT afterwards. That correction, and the `BootSequence::complete`
/// validation that makes the order load-bearing rather than decorative, are
/// the next commits — and this vector does not move again.
#[test]
fn the_boot_order_is_declared_and_every_step_says_why_it_is_there() {
    let names: Vec<&str> = BOOT_ORDER.iter().map(|step| step.name).collect();
    assert_eq!(
        names,
        vec![
            "identity",
            "keeper-admission",
            "coordinator-link",
            "session-reconcile",
            "ready"
        ],
        "the order is the architecture: identity before any mutation, the keeper \
         admitted once the coordinator's open-session set is in hand, readiness last"
    );
    for step in BOOT_ORDER {
        assert!(
            step.because.len() > 40,
            "{} carries no reason, so the next person cannot tell what breaks if it moves",
            step.name
        );
    }
}

/// The sequence records what actually ran, in the order it ran, and hands back
/// the declared reason for the step it just recorded.
#[test]
fn the_sequence_records_steps_in_the_order_they_complete() {
    let mut sequence = BootSequence::new();
    assert!(sequence.completed().is_empty());
    // The order here is the one `boot_sequence::run` ACTUALLY RECORDS, which
    // was always this and not the order the array declared — the test's own
    // name says "what actually ran", and its body had drifted to the
    // declaration instead. With `complete` refusing out-of-order steps it now
    // has to be right.
    for step in [
        StepId::Identity,
        StepId::KeeperAdmission,
        StepId::CoordinatorLink,
    ] {
        let because = sequence
            .complete(step)
            .expect("the executed order is the declared order");
        assert_eq!(because, BOOT_ORDER[step as usize].because);
    }
    assert_eq!(
        sequence.completed(),
        &[
            StepId::Identity,
            StepId::KeeperAdmission,
            StepId::CoordinatorLink
        ]
    );
}

/// THE GUARD F5 EXISTS FOR. `complete` accepted any order until this landed,
/// so the stop-time log printed whatever sequence it was handed while the
/// declaration beside it asserted a different one, and nothing noticed for as
/// long as both were wrong.
///
/// The assertion is against `StepId::ALL` and not against a list written out
/// again here, because a second list is a second thing to forget to update —
/// which is exactly how this file's oracle and `boot_sequence` came to
/// disagree.
#[test]
fn a_step_recorded_out_of_order_is_refused_rather_than_logged() {
    let mut sequence = BootSequence::new();
    sequence
        .complete(StepId::Identity)
        .expect("identity is first");

    let refusal = sequence
        .complete(StepId::CoordinatorLink)
        .expect_err("the link is not second; the keeper is");
    assert_eq!(refusal.step, StepId::CoordinatorLink);
    assert_eq!(
        refusal.recorded_names(),
        vec!["identity"],
        "the refusal names where boot actually was, so an operator reading it          knows which step is out of place rather than only that one is"
    );
    assert_eq!(
        sequence.completed(),
        &[StepId::Identity],
        "a refused step is NOT recorded: the log must not contain an order the \
         declaration does not allow"
    );

    // And the whole declared order is accepted, one at a time, end to end.
    let mut whole = BootSequence::new();
    for step in StepId::ALL {
        whole
            .complete(step)
            .unwrap_or_else(|refusal| panic!("{}: {refusal}", step.name()));
    }
    assert_eq!(
        whole.completed(),
        StepId::ALL.as_slice(),
        "a boot that ran the declared order records exactly it"
    );
}

/// A STEP ALREADY RECORDED IS NOT RECORDED AGAIN, and that is a different
/// refusal from an out-of-order one: it is a boot that reached `ready` twice,
/// not a boot that reached it early.
#[test]
fn a_step_recorded_twice_is_refused() {
    let mut sequence = BootSequence::new();
    sequence.complete(StepId::Identity).expect("first");
    assert!(
        sequence.complete(StepId::Identity).is_err(),
        "a second identity is not a stricter boot, it is a different one"
    );
}

/// The enum and the array are two artifacts that must move together, and
/// nothing in the type system says whether they do.
///
/// `StepId::name()` reads `BOOT_ORDER[step as usize]`, so the two can disagree
/// without a compiler, a lint or any other test noticing: the arm points at a
/// position, the position says something else, and every log line stays
/// well-formed while naming the wrong step.
///
/// WHAT THIS DOES NOT BUY, because a mutation proved it. Swapping two adjacent
/// `BOOT_ORDER` rows with the enum untouched — the exact reorder trap — leaves
/// the two in PERFECT AGREEMENT: the arm still points at index 1, index 1 now
/// spells a different name, and agreement says nothing about which is right.
/// This test passed under that mutation. The name vector in
/// `the_boot_order_is_declared_and_every_step_says_why_it_is_there` is what
/// caught it, because it is the only thing in the tree that knows the intended
/// order. So this is a CONSISTENCY check and the name vector is the CORRECTNESS
/// one; a test that only ever asks "do these two files agree" cannot catch
/// "both files are wrong together", which is what a reorder is.
#[test]
fn the_enum_and_the_array_agree_on_which_step_is_which() {
    let mut sequence = BootSequence::new();
    let mut names_from_the_enum = Vec::new();
    let mut reasons_from_the_enum = Vec::new();
    for step in StepId::ALL {
        names_from_the_enum.push(step.name());
        // The production accessor, not `BOOT_ORDER[step as usize].because`
        // written out again here: that would restate the very index
        // arithmetic under test and pass whatever the array says.
        reasons_from_the_enum.push(
            sequence
                .complete(step)
                .expect("StepId::ALL is the order complete accepts"),
        );
    }

    let names_from_the_array: Vec<&str> = BOOT_ORDER.iter().map(|step| step.name).collect();
    let reasons_from_the_array: Vec<&str> = BOOT_ORDER.iter().map(|step| step.because).collect();
    assert_eq!(
        (
            names_from_the_enum.as_slice(),
            reasons_from_the_enum.as_slice()
        ),
        (
            names_from_the_array.as_slice(),
            reasons_from_the_array.as_slice()
        ),
        "StepId and BOOT_ORDER disagree. The enum reads the array by position \
         (`step as usize`), so one of the two moved without the other. Enum \
         order: {names_from_the_enum:?}. Array order: {names_from_the_array:?}. \
         Every log line above is still well-formed and names the WRONG step: \
         re-add the step to the array and the arm to `name()` in the same \
         change, or the log will describe an order boot does not run."
    );
}

/// The two artifacts must also be the same LENGTH, which is a separate
/// failure from the ordering one: a step appended to the array with no
/// variant to reach it, or a variant with no row behind it.
///
/// A length mismatch is otherwise a panic deep inside `complete` on the first
/// real boot, named by a `StepId` debug string and nothing else.
#[test]
fn a_step_cannot_be_added_to_one_of_the_two_without_the_other() {
    assert_eq!(
        StepId::ALL.len(),
        BOOT_ORDER.len(),
        "StepId::ALL has {} variants and BOOT_ORDER has {} rows, so a step exists \
         in one artifact and not the other. `name()` indexes the array by \
         position, so the orphaned row is either unreachable (a variant with \
         no arm) or the wrong step's name (an arm pointing at someone else's \
         position).",
        StepId::ALL.len(),
        BOOT_ORDER.len()
    );
    // And no two rows claim the same name, which is what makes a partial
    // reorder visible: swapping two steps and copying one's name over the
    // other would leave the log printing a name twice and omitting it once,
    // with the order still plausible.
    for (index, row) in BOOT_ORDER.iter().enumerate() {
        let claimants: Vec<&str> = StepId::ALL
            .iter()
            .map(|step| step.name())
            .filter(|name| *name == row.name)
            .collect();
        assert_eq!(
            claimants.len(),
            1,
            "BOOT_ORDER[{index}] is named {:?} by {claimants:?}, so the log prints \
             that step's name {} times and another step's not at all. Boot runs \
             the array; the log names the enum; a step that shares a name with \
             another is a reorder that cannot be told apart from the one it was.",
            row.name,
            claimants.len()
        );
    }
}

/// v2 put reconciliation, the snapshot provider and readiness in one function
/// for a reason: a snapshot published before reconciliation describes a set the
/// coordinator has not confirmed, and acting on it closes live sessions.
#[test]
fn readiness_cannot_be_announced_before_the_reconcile_it_claims_to_describe() {
    let mut readiness = Readiness::default();
    assert!(!readiness.is_ready());

    let refused = readiness.advance(ReadyStep::MarkedReady).unwrap_err();
    assert_eq!(refused.step, ReadyStep::MarkedReady);
    assert_eq!(refused.at, Readiness::Starting);
    assert!(!readiness.is_ready());

    let refused = readiness
        .advance(ReadyStep::SnapshotProviderActivated)
        .unwrap_err();
    assert_eq!(refused.at, Readiness::Starting);
    assert!(
        !readiness.is_ready(),
        "a snapshot provider activated before reconciliation publishes a set the \
         coordinator never confirmed, and the coordinator closes what is missing"
    );
}

#[test]
fn readiness_advances_only_through_the_three_steps_in_order() {
    let mut readiness = Readiness::default();
    assert_eq!(
        readiness.advance(ReadyStep::Reconciled).unwrap(),
        Readiness::Reconciled
    );
    assert_eq!(
        readiness
            .advance(ReadyStep::SnapshotProviderActivated)
            .unwrap(),
        Readiness::SnapshotActive
    );
    assert_eq!(
        readiness.advance(ReadyStep::MarkedReady).unwrap(),
        Readiness::Ready
    );
    assert!(readiness.is_ready());
    // And it is a one-way door: a second announce is a second claim. The
    // refusal is pinned by variant, because `is_err()` would pass just as
    // happily for an advance that refused the right transition for a reason
    // that has nothing to do with ordering.
    assert!(matches!(
        readiness.advance(ReadyStep::MarkedReady),
        Err(OutOfOrderStep {
            step: ReadyStep::MarkedReady,
            at: Readiness::Ready,
        })
    ));
}

/// THE PIN F5 WAS MISSING, AND IT IS THE ONLY THING THAT MAKES THE REST OF
/// F5 LOAD-BEARING RATHER THAN DECORATIVE.
///
/// `BootSequence::complete` now refuses a step that is not next in
/// `StepId::ALL`, so the declared order is enforced — in the product, at
/// runtime. **But nothing on this branch ever executes `boot_sequence::run`,
/// so that enforcement is LATENT: a reordering of the two `complete` calls
/// turns nothing red here and refuses to boot on a machine instead. A guard
/// that is correct and dormant fails open, and open is the worse direction.
///
/// So this reads the composition root and compares its recorded order with the
/// declaration. It is a SOURCE-GREP test, and the shape deserves its reason
/// written down rather than left implicit: a test that reads source is noise
/// when the property is checkable by RUNNING the code, and this one is not.
/// `run` needs a `WorkerBoot`, a keeper, a coordinator and a door, none of
/// which a test binary should stand up to learn a lexical fact. The grep
/// asserts one true thing and cannot assert behaviour — and the behaviour it
/// would otherwise be standing in for is already enforced by the refusal. It
/// is a belt to a brace, and the brace is not decorative.
///
/// WHY THE ORACLE IS `StepId::ALL` AND NOT A LIST WRITTEN HERE. Three
/// statements of one order is where drift starts: the declaration, a literal
/// in a test, and `run` itself. This test reads the declaration and the code
/// and compares them to EACH OTHER, so it adds no fourth.
#[test]
fn the_boot_sequence_records_the_steps_in_the_declared_order() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/runtime/boot_sequence.rs"),
    )
    .expect("the boot sequence is a source file in this crate");

    let recorded: Vec<StepId> = source
        .lines()
        .filter_map(|line| line.trim().strip_prefix(".complete(StepId::"))
        .filter_map(|rest| rest.split(')').next())
        .filter_map(|name| match name.trim() {
            "Identity" => Some(StepId::Identity),
            "KeeperAdmission" => Some(StepId::KeeperAdmission),
            "CoordinatorLink" => Some(StepId::CoordinatorLink),
            "SessionReconcile" => Some(StepId::SessionReconcile),
            "Ready" => Some(StepId::Ready),
            other => panic!(
                "boot_sequence.rs records a step this test does not know: {other:?}. \
                 A new StepId variant has to be added here, or this pin silently \
                 stops covering the step it names."
            ),
        })
        .collect();

    assert_eq!(
        recorded,
        StepId::ALL.to_vec(),
        "the composition root records {:?} and BOOT_ORDER declares {:?}. \
         `BootSequence::complete` refuses anything that is not next in \
         `StepId::ALL`, so this order is enforced only at BOOT — and nothing \
         on this branch runs `boot_sequence::run`, which is why it is pinned \
         here rather than left to a machine to discover.",
        recorded,
        StepId::ALL
    );
}
