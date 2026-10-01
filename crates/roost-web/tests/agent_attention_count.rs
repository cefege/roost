//! The document title's attention count: the prefix it writes, and the number
//! behind it. The rules are pure functions over a status list and a ledger, so
//! they are pinned as rules rather than as the string a document happens to be
//! carrying. Mirrors
//! `crates/roost-web/src/components/notifications/agent_notifications/attention_count.rs`;
//! the acknowledgement half is `agent_attention.rs`.
//!
//! Test root, so the unwrap allowance is declared here (`CLAUDE.md` "the test
//! exemption reaches a test binary and not a fixture").

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_attention_support;

use agent_attention_support::{FOURTH, OTHER, THIRD, VIEWED, agent_status};
use roost_client_core::client::agents::AgentSeenLedger;
use roost_protocol::wire::AgentRuntimeState;
use roost_web::components::notifications::agent_notifications::attention_count::{
    attention_count, badge_title, base_title,
};

/// THE TITLE. One prefix, replaced rather than stacked, and gone on zero or when
/// the preference is off — the three states a reader actually sees in sequence.
#[test]
fn the_title_prefix_is_added_once_replaced_and_removed() {
    let base = base_title("Roost");
    let one = badge_title(&base, 1, true);
    assert_eq!(one, "(1) Roost");
    let two = badge_title(&base, 2, true);
    assert_eq!(
        two, "(2) Roost",
        "the count replaces the prefix it already wrote"
    );
    assert_eq!(
        two.matches('(').count(),
        1,
        "a second count must never stack"
    );
    assert_eq!(
        badge_title(&base_title(&two), 3, true),
        "(3) Roost",
        "re-reading the title still reads back as the bare title"
    );
    assert_eq!(
        badge_title(&base, 0, true),
        "Roost",
        "nothing owed leaves no prefix behind"
    );
    assert_eq!(
        badge_title(&base, 3, false),
        "Roost",
        "the preference is the reader's, and turning it off takes the prefix away"
    );
    assert_eq!(base_title(""), "Roost");
    assert_eq!(base_title("(2) Roost"), "Roost");
    assert_eq!(
        base_title("(Deploy) Roost"),
        "(Deploy) Roost",
        "a name that is not ours"
    );
    assert_eq!(
        base_title("(Roost"),
        "(Roost",
        "a paren that never closes is not a prefix"
    );
}

/// THE COUNT. Derived from the rows and the ledger, so it cannot be a second
/// stored answer: blocked while its own revision is unacknowledged, and finished
/// while its completion is.
#[test]
fn the_badge_counts_blocked_and_unacknowledged_completions_only() {
    let blocked = agent_status(VIEWED, AgentRuntimeState::Blocked, 2, 0);
    let finished = agent_status(OTHER, AgentRuntimeState::Idle, 4, 4);
    let running = agent_status(THIRD, AgentRuntimeState::Working, 5, 0);
    let settled = agent_status(FOURTH, AgentRuntimeState::Idle, 4, 4);

    // `settled` has to be ACKNOWLEDGED to be "already seen". A fresh ledger
    // leaves its completion unacknowledged, and an idle agent with a completion
    // this profile has not been told about is exactly what the badge is for —
    // the row reads Done and the title has to agree with it.
    let mut seen = AgentSeenLedger::new();
    seen.mark_seen(&settled);
    assert_eq!(
        attention_count([&blocked, &finished, &running, &settled].into_iter(), &seen),
        2,
        "a blocked agent and a completion this profile missed; a working row is \
         still running and an acknowledged completion is already news spent"
    );

    seen.mark_seen(&blocked);
    assert_eq!(
        attention_count([&blocked, &finished].into_iter(), &seen),
        1,
        "acknowledging a blocked row does not unblock the agent, so the badge \
         must stop counting it or it is a number nothing the reader does can move"
    );
    seen.mark_seen(&finished);
    assert_eq!(attention_count([&blocked, &finished].into_iter(), &seen), 0);
}

/// A blocked agent is very often still carrying the completion it earned on its
/// way to blocking. That is ONE thing wanting attention, and the count is a
/// per-row decision rather than a sum of two predicates — a title that reads
/// `(2)` over a single blocked agent is a number the reader cannot act on.
#[test]
fn a_blocked_agent_counts_once_even_when_it_carries_a_completion() {
    let blocked = agent_status(VIEWED, AgentRuntimeState::Blocked, 3, 2);
    let fresh = AgentSeenLedger::new();
    assert_eq!(
        attention_count([&blocked].into_iter(), &fresh),
        1,
        "its own revision is unacknowledged, so the row counts; the completion it \
 earned on the way there is the same agent, not a second one"
    );

    let mut seen = AgentSeenLedger::new();
    seen.mark_seen(&blocked);
    assert_eq!(
        attention_count([&blocked].into_iter(), &seen),
        0,
        "and once the operator has been shown it the row stops counting"
    );
}
