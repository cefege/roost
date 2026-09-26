//! What adopting a keeper survivor guarantees: the head comes from the keeper,
//! concurrent output past the staging bound refuses the adoption rather than
//! truncating it, a replacement core replays the history in order, and a resize
//! moves the history floor only as far as its replay bound did.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod session_support;

use std::sync::Arc;

use roost_keeper::history::HistoryRecord;
use roost_keeper::payloads::TerminalState;
use roost_worker::browser_commands::session_lifecycle::SessionOutcome;
use roost_worker::session::resume::{AdoptRefusal, SurvivorHistory};

use session_support::{Harness, SESSION, ScriptedKeeper, channel, session_id};

/// THE HEAD IS THE KEEPER'S. A history whose records do not sum to its head —
/// a geometry marker sits between two output records — is the case a
/// re-derivation gets wrong, and every absolute address after the marker moves.
#[test]
fn an_adoption_seeds_the_head_from_the_keeper_not_from_the_retained_bytes() {
    let keeper = Arc::new(ScriptedKeeper::with_survivor(7, 4242));
    *keeper.history.lock().expect("held") = SurvivorHistory {
        records: vec![
            HistoryRecord::Output {
                seq: 10,
                bytes: b"one".to_vec(),
            },
            HistoryRecord::Resize {
                seq: 12,
                cols: 80,
                rows: 24,
            },
            HistoryRecord::Output {
                seq: 13,
                bytes: b"three!".to_vec(),
            },
        ],
        head_seq: 13,
        base_cols: 80,
        base_rows: 24,
    };
    let harness = Harness::with_keeper(Arc::clone(&keeper));
    let adopted = harness
        .manager
        .adopt_survivor(&harness.adoption(SESSION, 7, "/home/user/project"))
        .expect("the survivor is adoptable");
    assert_eq!(
        adopted.head_seq, 13,
        "the head is what the keeper emitted, not the nine retained bytes"
    );
    assert_eq!(
        adopted.replay_offset, 4,
        "the floor is head minus retained, so a row address means the same thing"
    );
    let recorded = harness
        .table
        .with_record(&session_id(SESSION), |record| {
            (
                record.head_seq,
                record.history_floor(),
                record.scrollback.len(),
            )
        })
        .expect("the adopted session is live");
    assert_eq!(recorded, (13, 4, 9));
    assert_eq!(
        harness.sink.reserve(DurableEventKind::Closed).is_ok(),
        true,
        "the claim the adoption consumed is the one it was given"
    );
}

/// AN ADOPTION THAT FITS IS SWAPPED WHOLE, and the bytes the keeper produced
/// while the core was being rebuilt are parsed in order, behind the history.
#[test]
fn an_adoption_within_the_bound_replays_the_history_then_the_staged_bytes() {
    let keeper = Arc::new(ScriptedKeeper::with_survivor(7, 4242));
    *keeper.history.lock().expect("held") = SurvivorHistory {
        records: vec![HistoryRecord::Output {
            seq: 5,
            bytes: b"hello".to_vec(),
        }],
        head_seq: 5,
        base_cols: 80,
        base_rows: 24,
    };
    let harness = Harness::with_keeper(Arc::clone(&keeper));
    keeper.delivered().on_output(b"staged");
    let adopted = harness
        .manager
        .adopt_survivor(&harness.adoption(SESSION, 7, "/home/user/project"))
        .expect("five bytes is well inside the bound");
    assert_eq!(adopted.head_seq, 5);
    assert_eq!(
        harness.delivery.parsed.lock().expect("held").clone(),
        vec![b"staged".to_vec()],
        "the staged chunk is delivered whole, not trimmed and not dropped"
    );
    assert!(
        keeper.killed().is_empty(),
        "a survivor is not killed for fitting"
    );
    assert_eq!(
        harness
            .cells
            .lock()
            .expect("held")
            .installed
            .lock()
            .expect("held")
            .len(),
        1,
        "the adopted channel owes deltas, so its stream is installed once"
    );
}

/// the keeper's threshold and only then.
#[test]
fn three_stillborn_births_in_the_window_say_the_keeper_is_degraded() {
    let harness = Harness::new();
    for index in 0..3 {
        harness.install(
            &format!("00000000-0000-4000-8000-00000000000{index}"),
            7 + index as u16,
            "/home/user/project",
            "/home/user/project",
        );
        assert_eq!(
            harness
                .manager
                .kill_held_session(&session_id(&format!(
                    "00000000-0000-4000-8000-00000000000{index}"
                )))
                .expect("a held session"),
            SessionOutcome::Killed
        );
    }
    assert!(
        harness.manager.keeper_is_degraded(),
        "three pty children that printed nothing inside the window is a keeper fault"
    );
}

/// THE LOAD-BEARING REFUSAL. A survivor whose replayed history does not end at
/// the geometry the keeper reports cannot be adopted: the core would hold rows
/// numbered against a different width, and a client that merges them has no way
/// to notice. The assertions name BOTH geometries, so a handler that refuses
/// everything cannot pass this.
#[test]
fn a_survivor_whose_replay_does_not_converge_on_the_keepers_geometry_is_refused() {
    let keeper = Arc::new(ScriptedKeeper::with_survivor(7, 4242));
    *keeper.history.lock().expect("held") = SurvivorHistory {
        records: vec![
            HistoryRecord::Output {
                seq: 9,
                bytes: b"wide".to_vec(),
            },
            HistoryRecord::Resize {
                seq: 10,
                cols: 100,
                rows: 40,
            },
        ],
        head_seq: 10,
        base_cols: 80,
        base_rows: 24,
    };
    // The keeper still reports the geometry it had BEFORE the marker, so the
    // replay's last marker and the keeper's answer disagree.
    *keeper.applied.lock().expect("held") = TerminalState {
        applied_seq: 3,
        cols: 80,
        rows: 24,
    };
    let harness = Harness::with_keeper(Arc::clone(&keeper));
    let refused = harness
        .manager
        .adopt_survivor(&harness.adoption(SESSION, 7, "/home/user/project"))
        .expect_err("a history that does not converge is not adoptable");
    assert!(
        matches!(refused, AdoptRefusal::Unreplayable { channel: 7, .. }),
        "the refusal is the replay, not the staging: {refused}"
    );
    let message = refused.to_string();
    assert!(
        message.contains("100x40") && message.contains("80x24"),
        "the message names both geometries so an operator can see the divergence: {message}"
    );
    assert_eq!(
        harness
            .table
            .with_record(&session_id(SESSION), |record| record.head_seq),
        None,
        "nothing is installed, so no client can be handed rows numbered against \
         the wrong width"
    );
    assert_eq!(
        keeper.killed(),
        vec![7],
        "the survivor dies rather than living half-adopted"
    );
}

/// THE ADMITTING TWIN of the test above, because a handler that always refuses
/// passes a suite of refusals. A history that DOES converge is adopted.
#[test]
fn a_survivor_whose_replay_converges_on_the_keepers_geometry_is_admitted() {
    let keeper = Arc::new(ScriptedKeeper::with_survivor(7, 4242));
    *keeper.history.lock().expect("held") = SurvivorHistory {
        records: vec![
            HistoryRecord::Output {
                seq: 9,
                bytes: b"wide".to_vec(),
            },
            HistoryRecord::Resize {
                seq: 10,
                cols: 100,
                rows: 40,
            },
        ],
        head_seq: 10,
        base_cols: 80,
        base_rows: 24,
    };
    *keeper.applied.lock().expect("held") = TerminalState {
        applied_seq: 3,
        cols: 100,
        rows: 40,
    };
    let harness = Harness::with_keeper(Arc::clone(&keeper));
    let adopted = harness
        .manager
        .adopt_survivor(&harness.adoption(SESSION, 7, "/home/user/project"))
        .expect("a history that converges is adoptable");
    assert_eq!(adopted.head_seq, 10);
    assert_eq!(
        adopted.replay_offset, 6,
        "the floor is head minus the four retained bytes"
    );
    let geometry = harness
        .table
        .with_record(&session_id(SESSION), |record| {
            (record.terminal_core.cols(), record.terminal_core.rows())
        })
        .expect("the survivor is live at the geometry it was replayed at");
    assert_eq!(geometry, (100, 40));
    assert!(
        keeper.killed().is_empty(),
        "a survivor that fits is not killed"
    );
}

/// A KEEPER THAT WILL NOT ANSWER ITS CHANNEL LIST is a refusal naming the
/// keeper's reason, not a silent "no survivor": the two are different incidents
/// with different operator responses.
#[test]
fn a_channel_list_that_cannot_be_read_refuses_with_the_keeper_s_reason() {
    let keeper = Arc::new(ScriptedKeeper::with_survivor(7, 4242));
    *keeper.list_fails.lock().expect("held") = true;
    let harness = Harness::with_keeper(Arc::clone(&keeper));
    let refused = harness
        .manager
        .adopt_survivor(&harness.adoption(SESSION, 7, "/home/user/project"))
        .expect_err("an unreadable channel list cannot be adopted from");
    assert!(
        matches!(refused, AdoptRefusal::Unreplayable { .. }),
        "an unreachable keeper is a replay failure, not an absent survivor: {refused}"
    );
    assert!(
        refused.to_string().contains("the socket went away"),
        "the message carries the keeper's reason: {refused}"
    );
    assert!(
        keeper.killed().is_empty(),
        "a channel this worker never reached is not killed for asking"
    );
}

/// A SESSION WITH NO SURVIVOR IS REFUSED, NOT RE-CREATED. The boundary between
/// "adopt what the keeper still holds" and "open a fresh child" is this
/// refusal, and `docs/FAILURE-INDEX.md` has an entry for what happens when the
/// other side of it is taken by accident.
#[test]
fn a_session_with_no_keeper_channel_is_refused_rather_than_recreated() {
    let harness = Harness::new();
    let refused = harness
        .manager
        .adopt_survivor(&harness.adoption(SESSION, 7, "/home/user/project"))
        .expect_err("there is no survivor to adopt");
    assert_eq!(
        refused,
        AdoptRefusal::NoSurvivor(7),
        "the refusal names the channel, and it is the absence one: {refused}"
    );
    assert!(harness.table.is_empty_session(&session_id(SESSION)));
    assert!(
        harness.sink.published().is_empty(),
        "a refused adoption publishes nothing: the caller respawns or reports"
    );
}
