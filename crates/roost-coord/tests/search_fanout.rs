//! Coordinator-authorized global terminal search through fake routable worker
//! generations and the real pending table: fan-out caps, budgets, and the
//! typed per-session partials.
//!
//! Ported from `apps/coord/tests/workers/worker-ws-transport-global-search.test.ts`;
//! cursor continuation is `search_fanout_cursor.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
#[path = "search_support/mod.rs"]
mod support;

use std::collections::{HashMap, HashSet};

use roost_proto::{GlobalSearchPartialReason, SessionsSearchGlobalRequest};
use roost_protocol::terminal_search::{
    GLOBAL_TERMINAL_SEARCH_MAX_MATCHES, GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
    GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS, GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
};
use serde_json::{Value, json};
use support::{Captured, Harness, WORKER_A1, WORKER_A2, ok_entry, search_request, session_id};

fn session_ids(command: &Captured) -> Vec<String> {
    command.control["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|session| session["session_id"].as_str().unwrap().to_owned())
        .collect()
}

// "enumerates only the newest 32 authorized open sessions and sends one capped batch per worker"
#[tokio::test]
async fn only_the_newest_page_of_open_sessions_is_sent_one_capped_batch_per_worker() {
    let harness = Harness::new("fanout-caps").await;
    let worker_a1 = harness.install_worker(WORKER_A1);
    let worker_a2 = harness.install_worker(WORKER_A2);
    for index in 1..=35 {
        let worker = if index == 35 { WORKER_A2 } else { WORKER_A1 };
        harness
            .open_session(&session_id(index), worker, i64::from(index))
            .await;
    }
    harness
        .insert_session(&session_id(101), WORKER_A1, "closed", 101)
        .await;
    let session_cap = u32::try_from(GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS).unwrap();
    let answer = harness.search(SessionsSearchGlobalRequest {
        query: "needle".to_owned(),
        case_sensitive: true,
        search_id: "fanout-caps".to_owned(),
        max_sessions: session_cap + 100,
        max_rows_per_session: GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION + 100,
        max_matches: GLOBAL_TERMINAL_SEARCH_MAX_MATCHES + 100,
        ..Default::default()
    });
    let commands = [
        worker_a1
            .wait_for_kind("search-scrollback-batch", 1)
            .await
            .remove(0),
        worker_a2
            .wait_for_kind("search-scrollback-batch", 1)
            .await
            .remove(0),
    ];
    let mut requested = Vec::new();
    for command in &commands {
        assert_eq!(command.browser_id, "global-browser:global-tab");
        let control = &command.control;
        assert_eq!(control["search_id"], "fanout-caps");
        assert_eq!(control["query"], "needle");
        assert_eq!(control["case_sensitive"], true);
        assert_eq!(
            control["max_rows_per_session"],
            GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION
        );
        assert_eq!(
            control["deadline_ms"],
            GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS
        );
        assert!(control.get("regex").is_none());
        for session in control["sessions"].as_array().unwrap() {
            assert_eq!(session["grid_epoch"], "");
            assert!(session.get("before_row").is_none());
        }
        let ids = session_ids(command);
        assert_eq!(control["max_matches"], json!(ids.len() * 8));
        requested.extend(ids);
    }
    assert_eq!(requested.len(), GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS);
    assert_eq!(
        requested.iter().collect::<HashSet<_>>().len(),
        GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS
    );
    for skipped in [session_id(1), session_id(2), session_id(3), session_id(101)] {
        assert!(
            !requested.contains(&skipped),
            "{skipped} is outside the page"
        );
    }
    let budget: u64 = commands
        .iter()
        .map(|c| c.control["max_matches"].as_u64().unwrap())
        .sum();
    assert_eq!(budget, u64::from(GLOBAL_TERMINAL_SEARCH_MAX_MATCHES));

    for command in &commands {
        let entries: Vec<Value> = session_ids(command)
            .iter()
            .map(|id| ok_entry(id, None, json!({})))
            .collect();
        harness.respond(command, json!({ "entries": entries }));
    }
    let response = answer.await.unwrap().unwrap();
    assert_eq!(response.eligible_sessions, 35);
    assert_eq!(response.searched_sessions, session_cap);
    assert_eq!(response.matches.len(), GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS);
    assert!(response.partials.is_empty());
    assert_eq!(response.next_cursor, None);
    // 35 sessions are authorized and 32 fit one page: coverage is partial,
    // not "32 of 32 searched".
    assert!(response.truncated);
    assert_eq!(harness.pending(), 0);
}

// "defers zero-budget workers and preserves cumulative progress"
#[tokio::test]
async fn zero_budget_workers_are_deferred_and_progress_accumulates() {
    let harness = Harness::new("fanout-defer").await;
    let first_worker = harness.install_worker(WORKER_A1);
    let deferred_worker = harness.install_worker(WORKER_A2);
    harness.open_session(&session_id(121), WORKER_A2, 1).await;
    harness.open_session(&session_id(122), WORKER_A1, 2).await;
    let request = SessionsSearchGlobalRequest {
        query: "needle".to_owned(),
        search_id: "caller-limits".to_owned(),
        max_sessions: 2,
        max_rows_per_session: 17,
        max_matches: 1,
        ..Default::default()
    };
    let first = harness.search(request.clone());
    let sent = first_worker
        .wait_for_kind("search-scrollback-batch", 1)
        .await;
    assert_eq!(sent[0].control["max_rows_per_session"], 17);
    assert_eq!(sent[0].control["max_matches"], 1);
    assert!(deferred_worker.commands().is_empty());
    let first_id = session_ids(&sent[0]).remove(0);
    harness.respond(
        &sent[0],
        json!({ "entries": [ok_entry(&first_id, Some("epoch-first"), json!({
        "matches": [{ "row": 20, "col": 1, "len": 6, "preview": "needle" }],
        "scrollback_total": 30, "scanned_start_row": 13, "scanned_end_row": 30,
        "next_before_row": 13, "stop_reason": "row_limit",
    }))] }),
    );
    let first = first.await.unwrap().unwrap();
    assert_eq!(first.eligible_sessions, 2);
    assert_eq!(first.searched_sessions, 1);
    assert!(first.next_cursor.is_some());

    let second = harness.search(SessionsSearchGlobalRequest {
        cursor: first.next_cursor,
        ..request
    });
    let sent = deferred_worker
        .wait_for_kind("search-scrollback-batch", 1)
        .await;
    assert_eq!(sent[0].control["max_matches"], 1);
    assert_eq!(first_worker.commands().len(), 1);
    let second_id = session_ids(&sent[0]).remove(0);
    harness.respond(
        &sent[0],
        json!({ "entries": [ok_entry(&second_id, None, json!({}))] }),
    );
    let second = second.await.unwrap().unwrap();
    assert_eq!(second.eligible_sessions, 2);
    assert_eq!(second.searched_sessions, 2);
    assert!(second.next_cursor.is_some());
}

// "returns every typed per-session failure without turning the page into empty success"
#[tokio::test]
async fn every_typed_per_session_failure_is_returned_as_a_partial() {
    let harness = Harness::new("fanout-partials").await;
    let worker = harness.install_worker(WORKER_A1);
    for index in 0..6 {
        harness
            .open_session(&session_id(200 + index), WORKER_A1, i64::from(index))
            .await;
    }
    let unavailable_id = session_id(299);
    harness.open_session(&unavailable_id, WORKER_A2, 99).await;
    let answer = harness.search(search_request("partial", "partial-families", None));
    let sent = worker.wait_for_kind("search-scrollback-batch", 1).await;
    let entries: Vec<Value> = session_ids(&sent[0])
        .iter()
        .map(|id| match &id[id.len() - 3..] {
            "200" => ok_entry(id, Some("deadline-epoch"), json!({
                "matches": [], "truncated": true, "scanned_start_row": 4, "scanned_end_row": 10, "stop_reason": "deadline",
            })),
            "201" => json!({ "status": "error", "session_id": id, "error": "epoch_changed" }),
            "202" => ok_entry(id, Some("match-epoch"), json!({
                "truncated": true, "scanned_start_row": 5, "stop_reason": "match_limit",
            })),
            "203" => ok_entry(id, Some("floor-epoch"), json!({ "scanned_start_row": 5, "history_floor": "evicted" })),
            "204" => json!({ "status": "error", "session_id": id, "error": "session_closed" }),
            _ => json!({ "status": "error", "session_id": id, "error": "internal" }),
        })
        .collect();
    harness.respond(&sent[0], json!({ "entries": entries }));
    let response = answer.await.unwrap().unwrap();
    let reasons: HashMap<String, GlobalSearchPartialReason> = response
        .partials
        .iter()
        .map(|partial| {
            (
                partial.session_id.clone(),
                partial.reason.as_known().unwrap(),
            )
        })
        .collect();
    let families: HashSet<GlobalSearchPartialReason> = reasons.values().copied().collect();
    assert_eq!(
        families,
        HashSet::from([
            GlobalSearchPartialReason::WorkerUnavailable,
            GlobalSearchPartialReason::Deadline,
            GlobalSearchPartialReason::EpochChanged,
            GlobalSearchPartialReason::MatchLimit,
            GlobalSearchPartialReason::HistoryEvicted,
            GlobalSearchPartialReason::SessionClosed,
            GlobalSearchPartialReason::MalformedResult,
        ])
    );
    assert_eq!(
        reasons.get(&unavailable_id),
        Some(&GlobalSearchPartialReason::WorkerUnavailable)
    );
    assert!(response.truncated);
    assert!(response.next_cursor.is_some());
    assert_eq!(response.eligible_sessions, 7);
    assert_eq!(response.searched_sessions, 6);
    assert!(!response.matches.is_empty());
}
