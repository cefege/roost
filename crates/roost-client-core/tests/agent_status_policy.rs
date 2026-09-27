//! What a retained status READS as: the level, its copy, its priority, and the
//! acknowledgement that turns a finished agent from "idle" into "done".
//!
//! The level is derived, not stored, so this is where the derivation is
//! exercised — including the rule that an identified occupant's first
//! completion is unseen while a legacy deployment's is not, and the rule that a
//! replacement occupant's revision 1 does not re-show the previous agent's
//! completion.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::client::agents::{
    AGENT_SEEN_STORAGE_KEY, AgentDotStatus, AgentSeenLedger, AgentStatusLevel,
    agent_status_completion_unseen, agent_status_occupant_key, agent_status_revision_token,
    agent_status_tooltip, derive_agent_status_level, fold_agent_status_levels,
    format_agent_status_counts, matches_agent_status_revision_token,
};
use roost_protocol::wire::{
    AgentId, AgentOccupantId, AgentRuntimeState, AgentStatus, AgentStatusFields, AgentStatusSource,
    AgentStatusUpdate, SessionId, StatusEpoch,
};

const SESSION: &str = "30000000-0000-4000-8000-000000000030";
const EPOCH: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const OCCUPANT: &str = "11111111-aaaa-4aaa-8aaa-111111111111";

fn status(revision: i64, completed_revision: i64, state: AgentRuntimeState) -> AgentStatus {
    AgentStatus {
        common: AgentStatusFields {
            session_id: SessionId::try_from(SESSION).expect("a session id"),
            agent_id: AgentId::try_from("omp").expect("an agent id"),
            state,
            message: None,
            revision,
            completed_revision,
            updated_at: 1_800_000_000_000,
            status_epoch: Some(StatusEpoch::try_from(EPOCH).expect("an epoch")),
            occupant_id: Some(AgentOccupantId::try_from(OCCUPANT).expect("an occupant")),
            source: Some(AgentStatusSource::Integration),
            occupant_exited: false,
        },
        active: true,
    }
}

fn legacy(revision: i64, completed_revision: i64) -> AgentStatus {
    let mut value = status(revision, completed_revision, AgentRuntimeState::Idle);
    value.common.status_epoch = None;
    value.common.occupant_id = None;
    value.common.source = None;
    value
}

#[test]
fn an_unseen_completion_of_an_identified_occupant_reads_done_and_a_seen_one_reads_idle() {
    let done = status(9, 9, AgentRuntimeState::Idle);
    assert_eq!(
        derive_agent_status_level(Some(&done), None),
        AgentStatusLevel::Done
    );
    assert_eq!(
        derive_agent_status_level(Some(&done), Some(9)),
        AgentStatusLevel::Idle,
        "an acknowledged completion is not news"
    );
    assert!(agent_status_completion_unseen(&done, Some(8)));
    assert!(!agent_status_completion_unseen(&done, Some(9)));
}

#[test]
fn a_legacy_status_never_reads_done_because_it_could_not_have_been_missed() {
    let legacy = legacy(4, 4);
    assert_eq!(
        derive_agent_status_level(Some(&legacy), None),
        AgentStatusLevel::Idle,
        "a legacy deployment's completion predates this profile, so reporting it \
         as news would be a Done nobody can ever clear"
    );
    assert!(!agent_status_completion_unseen(&legacy, None));
}

#[test]
fn blocked_and_working_outrank_a_completion() {
    let blocked = status(9, 9, AgentRuntimeState::Blocked);
    assert_eq!(
        derive_agent_status_level(Some(&blocked), None),
        AgentStatusLevel::Blocked,
        "an agent waiting for a human is not done, whatever it completed"
    );
    let working = status(9, 9, AgentRuntimeState::Working);
    assert_eq!(
        derive_agent_status_level(Some(&working), None),
        AgentStatusLevel::Working
    );
    assert_eq!(
        derive_agent_status_level(None, None),
        AgentStatusLevel::Unknown
    );
}

#[test]
fn a_replacement_occupants_first_report_does_not_replay_the_previous_completion() {
    let previous = status(90, 90, AgentRuntimeState::Idle);
    let mut replacement = status(1, 1, AgentRuntimeState::Idle);
    replacement.common.occupant_id = Some(
        AgentOccupantId::try_from("22222222-aaaa-4aaa-8aaa-222222222222").expect("an occupant"),
    );
    let mut ledger = AgentSeenLedger::new();
    ledger.mark_seen(&previous);

    // `previous` KEEPS ITS OWN occupant key. Re-pointing it at the
    // replacement's made the lookup below ask a key nothing was ever stored
    // under, so the assertion read the default floor rather than the
    // acknowledgement — and the sentence it carries, "the previous occupant's
    // own revision is acknowledged under its own key", is only true while the
    // two occupants have different keys. That difference is the whole subject
    // of this test.
    assert_ne!(
        previous.common.occupant_id, replacement.common.occupant_id,
        "the two occupants must be keyed differently or this test proves nothing"
    );
    assert_eq!(
        derive_agent_status_level(
            Some(&replacement),
            Some(ledger.acknowledged_revision(&replacement))
        ),
        AgentStatusLevel::Done,
        "the replacement has not been acknowledged, so ITS completion is unseen"
    );
    assert_eq!(
        derive_agent_status_level(
            Some(&previous),
            Some(ledger.acknowledged_revision(&previous))
        ),
        AgentStatusLevel::Idle,
        "the previous occupant's own revision is acknowledged under its own key"
    );
}

#[test]
fn the_ledger_survives_a_round_trip_through_storage_and_merges_a_second_tabs_write() {
    let mut ledger = AgentSeenLedger::new();
    let done = status(9, 9, AgentRuntimeState::Idle);
    assert!(ledger.mark_seen(&done));
    assert!(
        !ledger.mark_seen(&done),
        "acknowledging twice moves nothing"
    );

    let raw = ledger.encode();
    assert!(!raw.is_empty());
    let mut restored = AgentSeenLedger::decode(Some(&raw));
    assert_eq!(
        restored.acknowledged_revision(&done),
        9,
        "a stored acknowledgement must read back as one"
    );

    // A second tab acknowledged a LATER revision while this one was away.
    let mut later = status(12, 12, AgentRuntimeState::Idle);
    later.common.occupant_id = done.common.occupant_id.clone();
    let mut other_tab = AgentSeenLedger::new();
    other_tab.mark_seen(&later);
    assert!(restored.merge(&other_tab.tokens()));
    assert_eq!(
        restored.acknowledged_revision(&later),
        12,
        "merging keeps the highest revision per occupant, so another tab's \
         write is not undone by this one's"
    );
    // Compared against a ledger that ALSO acknowledged 12, not against the
    // one that only ever saw 9. The merge is supposed to raise this tab's
    // record; asserting equality with the pre-merge ledger asserted the
    // opposite of the sentence two lines above it.
    let mut both = ledger.clone();
    assert!(both.mark_seen(&later));
    assert_eq!(restored.encode(), both.encode());
    assert!(AGENT_SEEN_STORAGE_KEY.starts_with("roost."));
}

#[test]
fn a_malformed_stored_record_is_dropped_rather_than_read_as_an_acknowledgement() {
    let raw = "\u{1f}broken\nnot-a-session\u{1f}5\u{1f}\u{1f}\u{1f}";
    let ledger = AgentSeenLedger::decode(Some(raw));
    assert!(ledger.is_empty());
    let undecided = status(3, 3, AgentRuntimeState::Idle);
    assert_eq!(
        ledger.acknowledged_revision(&undecided),
        -1,
        "a dropped record acknowledges nothing"
    );
}

#[test]
fn a_revision_token_only_matches_its_own_occupant() {
    let done = status(9, 9, AgentRuntimeState::Idle);
    let token = agent_status_revision_token(&done);
    assert!(matches_agent_status_revision_token(&done, &token));
    let mut replacement = status(9, 9, AgentRuntimeState::Idle);
    replacement.common.occupant_id = Some(
        AgentOccupantId::try_from("22222222-aaaa-4aaa-8aaa-222222222222").expect("an occupant"),
    );
    assert!(
        !matches_agent_status_revision_token(&replacement, &token),
        "two occupants of one session both reach revision 9, so a bare revision \
         would match the wrong agent"
    );
    assert_eq!(
        agent_status_occupant_key(&done.common),
        Some(format!("{EPOCH}:{OCCUPANT}"))
    );
    assert_eq!(agent_status_occupant_key(&legacy(1, 1).common), None);
}

#[test]
fn a_rollup_reports_the_highest_level_present_and_ignores_unknown_in_the_total() {
    let rollup = fold_agent_status_levels([
        AgentStatusLevel::Unknown,
        AgentStatusLevel::Idle,
        AgentStatusLevel::Working,
        AgentStatusLevel::Done,
    ]);
    // `done` outranks `working`: a completion this profile has not seen is the
    // one thing on the chip that is news, so it is what the chip leads with.
    // v2 assigns those priorities 3 and 2 in `AGENT_STATUS_PRESENTATION` and
    // folds on exactly that comparison, so this is the port's behaviour and the
    // expectation that was here was the defect.
    assert_eq!(rollup.level, AgentStatusLevel::Done);
    assert_eq!(rollup.total, 3);
    assert_eq!(rollup.counts.idle, 1);
    assert_eq!(rollup.counts.unknown, 1);

    let empty = fold_agent_status_levels([]);
    assert_eq!(empty.level, AgentStatusLevel::Unknown);
    assert_eq!(empty.total, 0);

    assert_eq!(
        format_agent_status_counts(&rollup.counts),
        "1 working · 1 done · 1 idle",
        "a rollup reads in attention order, not in the order it was folded"
    );
}

#[test]
fn a_tooltip_is_the_levels_own_words_plus_the_agents_message() {
    let mut with_message = status(9, 9, AgentRuntimeState::Blocked);
    with_message.common.message = Some("  waiting on a test  ".to_owned());
    let tooltip = agent_status_tooltip(&with_message, None);
    assert!(tooltip.starts_with("The agent is waiting for your input"));
    assert!(tooltip.ends_with("waiting on a test"));

    let bare = status(9, 9, AgentRuntimeState::Working);
    assert_eq!(
        agent_status_tooltip(&bare, None),
        "The agent is working",
        "a status with no message reads as the level alone, with no dangling \
         separator"
    );
}

#[test]
fn every_level_has_a_presentation_and_a_dot_status() {
    for level in [
        AgentStatusLevel::Blocked,
        AgentStatusLevel::Done,
        AgentStatusLevel::Working,
        AgentStatusLevel::Idle,
        AgentStatusLevel::Unknown,
    ] {
        let presentation = roost_client_core::client::agents::agent_status_presentation(level);
        assert!(!presentation.label.is_empty());
        assert!(!presentation.count_label.is_empty());
        assert!(!presentation.tooltip.is_empty());
        assert!(
            presentation.color.starts_with("var(--md-"),
            "a raw colour here would be a second palette outside the design system"
        );
        let _ = presentation.dot_status;
    }
    assert_eq!(
        roost_client_core::client::agents::agent_status_presentation(AgentStatusLevel::Done)
            .dot_status,
        AgentDotStatus::Ok
    );
}

#[test]
fn an_update_that_fails_its_own_wire_check_is_not_retained() {
    // The projection validates before it fences, so a frame that could never
    // have been a status cannot become one.
    let invalid = AgentStatusUpdate {
        common: AgentStatusFields {
            session_id: SessionId::try_from(SESSION).expect("a session id"),
            agent_id: AgentId::try_from("omp").expect("an agent id"),
            state: AgentRuntimeState::Idle,
            message: None,
            revision: 5,
            completed_revision: 9,
            updated_at: 1_800_000_000_000,
            status_epoch: Some(StatusEpoch::try_from(EPOCH).expect("an epoch")),
            // A half-present identity is not a legacy status either.
            occupant_id: None,
            source: Some(AgentStatusSource::Integration),
            occupant_exited: false,
        },
        active: true,
    };
    let seen = AgentSeenLedger::new();
    let mut projection = roost_client_core::client::agents::AgentStatusProjection::new();
    assert_eq!(projection.apply_update(&invalid, &seen), None);
    assert!(projection.is_empty());
}
