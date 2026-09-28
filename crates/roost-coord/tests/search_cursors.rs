//! Global-search cursor binding, expiry, eviction and cancellation ordering,
//! per device and per process.
//!
//! Ported from the "global search cursor owner" block of
//! `apps/coord/tests/search/global-search-cursors.test.ts`. Every test owns
//! its owner and clock, so no cursor leaks between cases.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use roost_coord::search::cursor_types::{
    CursorIssueRefusal, GlobalSearchAdmission, GlobalSearchContinuation, GlobalSearchCursorBinding,
    GlobalSearchCursorIssue, GlobalSearchCursorProgress, GlobalSearchIdentity,
    GlobalSearchSessionPosition,
};
use roost_coord::search::cursors::{
    GLOBAL_SEARCH_MAX_ACTIVE_PER_DEVICE, GLOBAL_SEARCH_MAX_CANCEL_TOMBSTONES_PER_DEVICE,
    GlobalSearchCursorOwner,
};
use roost_coord::search::options::GlobalSearchPageLimits;
use roost_protocol::terminal_search::{
    GLOBAL_TERMINAL_SEARCH_CURSOR_TTL_MS, GLOBAL_TERMINAL_SEARCH_MAX_CURSORS_PER_DEVICE,
    GLOBAL_TERMINAL_SEARCH_MAX_MATCHES, GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
    GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
};

const LIMITS: GlobalSearchPageLimits = GlobalSearchPageLimits {
    max_sessions: GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
    max_rows_per_session: GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
    max_matches: GLOBAL_TERMINAL_SEARCH_MAX_MATCHES,
};

fn identity(search_id: &str) -> GlobalSearchIdentity {
    GlobalSearchIdentity {
        device_fingerprint: "device-a".to_owned(),
        tab_id: "tab-a".to_owned(),
        search_id: search_id.to_owned(),
    }
}

fn binding(search_id: &str) -> GlobalSearchCursorBinding {
    GlobalSearchCursorBinding {
        identity: identity(search_id),
        query: "needle".to_owned(),
        case_sensitive: false,
        limits: LIMITS,
    }
}

fn position() -> GlobalSearchSessionPosition {
    GlobalSearchSessionPosition {
        session_id: "00000000-0000-4000-8000-000000000001".to_owned(),
        worker_fp: "a".repeat(64),
        grid_epoch: "epoch-a".to_owned(),
        before_row: Some(2_048),
    }
}

fn issue(
    binding: GlobalSearchCursorBinding,
    continuations: Vec<(GlobalSearchSessionPosition, bool, Option<u64>)>,
    eligible_sessions: usize,
    searched: Vec<String>,
) -> GlobalSearchCursorIssue {
    GlobalSearchCursorIssue {
        binding,
        continuations: continuations
            .into_iter()
            .map(
                |(position, searched, requested_before_row)| GlobalSearchContinuation {
                    position,
                    searched,
                    requested_before_row,
                },
            )
            .collect(),
        eligible_sessions,
        searched_session_ids: searched,
    }
}

fn unvisited_issue(
    search_id: &str,
    position: GlobalSearchSessionPosition,
) -> GlobalSearchCursorIssue {
    issue(
        binding(search_id),
        vec![(position, false, None)],
        1,
        Vec::new(),
    )
}

// "binds opaque cursors to device, tab, search, and options"
#[test]
fn a_cursor_is_bound_to_device_tab_search_and_options_and_claimed_once() {
    let owner = GlobalSearchCursorOwner::new();
    let base = binding("search-a");
    let cursor = owner
        .issue_cursor(issue(
            base.clone(),
            vec![(position(), false, None)],
            1,
            vec![position().session_id],
        ))
        .unwrap();
    let shape: Vec<usize> = cursor.split('-').map(str::len).collect();
    assert_eq!(shape, [8, 4, 4, 4, 12]);
    assert_eq!(&cursor[14..15], "4", "a v4 token");
    let mut mismatches = Vec::new();
    for edit in 0..8 {
        let mut other = base.clone();
        match edit {
            0 => other.identity.device_fingerprint = "device-b".to_owned(),
            1 => other.identity.tab_id = "tab-b".to_owned(),
            2 => other.identity.search_id = "search-b".to_owned(),
            3 => other.query = "other".to_owned(),
            4 => other.case_sensitive = true,
            5 => other.limits.max_sessions = 1,
            6 => other.limits.max_rows_per_session = 1,
            _ => other.limits.max_matches = 1,
        }
        mismatches.push(other);
    }
    for mismatch in &mismatches {
        assert_eq!(owner.claim_cursor(&cursor, mismatch), None);
    }
    assert_eq!(
        owner.claim_cursor(&cursor, &base),
        Some(GlobalSearchCursorProgress {
            sessions: vec![position()],
            eligible_sessions: 1,
            searched_session_ids: vec![position().session_id],
        })
    );
    assert_eq!(owner.claim_cursor(&cursor, &base), None);
}

// "expires at sixty seconds and evicts the oldest fifth cursor per device"
#[test]
fn cursors_expire_after_the_ttl_and_the_oldest_past_the_device_cap_is_evicted() {
    let now = Arc::new(AtomicI64::new(10_000));
    let minted = Arc::new(AtomicUsize::new(0));
    let clock = Arc::clone(&now);
    let counter = Arc::clone(&minted);
    let owner = GlobalSearchCursorOwner::with_sources(
        Arc::new(move || clock.load(Ordering::SeqCst)),
        Arc::new(move || Ok(format!("token-{}", counter.fetch_add(1, Ordering::SeqCst)))),
    );
    let cap = GLOBAL_TERMINAL_SEARCH_MAX_CURSORS_PER_DEVICE as usize;
    for index in 0..=cap {
        let session = GlobalSearchSessionPosition {
            session_id: format!("00000000-0000-4000-8000-{:012}", index + 20),
            ..position()
        };
        owner
            .issue_cursor(unvisited_issue(&format!("search-{index}"), session))
            .unwrap();
    }
    assert_eq!(owner.claim_cursor("token-0", &binding("search-0")), None);
    assert!(
        owner
            .claim_cursor("token-1", &binding("search-1"))
            .is_some()
    );

    let expiring = owner
        .issue_cursor(unvisited_issue("expires", position()))
        .unwrap();
    now.fetch_add(
        i64::from(GLOBAL_TERMINAL_SEARCH_CURSOR_TTL_MS),
        Ordering::SeqCst,
    );
    assert_eq!(owner.claim_cursor(&expiring, &binding("expires")), None);
}

// "retires before dispatch and releases in-flight cancellation only after send"
#[test]
fn cancellation_retires_first_and_releases_the_search_only_on_completion() {
    let now = Arc::new(AtomicI64::new(1));
    let clock = Arc::clone(&now);
    let owner = GlobalSearchCursorOwner::with_sources(
        Arc::new(move || clock.load(Ordering::SeqCst)),
        Arc::new(|| Ok("cursor-token".to_owned())),
    );
    let search = identity("search-a");
    assert_eq!(owner.begin_search(&search), GlobalSearchAdmission::Started);
    assert!(owner.select_sessions(&search, &[position()]));
    let released = owner.on_cancel(&search);
    let cursor = owner
        .issue_cursor(unvisited_issue("search-a", position()))
        .unwrap();

    let prepared = owner.prepare_cancellation(&search);
    assert!(prepared.should_dispatch);
    assert_eq!(prepared.selected_sessions, vec![position()]);
    assert_eq!(
        owner.begin_search(&search),
        GlobalSearchAdmission::Cancelled
    );
    assert_eq!(owner.claim_cursor(&cursor, &binding("search-a")), None);
    assert!(
        !released.is_cancelled(),
        "released only after the cancels were sent"
    );
    owner.complete_cancellation(&search);
    assert!(released.is_cancelled());
    assert!(!owner.prepare_cancellation(&search).should_dispatch);

    now.fetch_add(
        i64::from(GLOBAL_TERMINAL_SEARCH_CURSOR_TTL_MS),
        Ordering::SeqCst,
    );
    assert_eq!(owner.begin_search(&search), GlobalSearchAdmission::Started);
}

// "allows an unvisited cursor but rejects a row cursor without an epoch"
#[test]
fn an_unvisited_cursor_is_allowed_but_a_row_without_an_epoch_or_a_repeat_is_not() {
    let owner = GlobalSearchCursorOwner::new();
    let unvisited = GlobalSearchSessionPosition {
        grid_epoch: String::new(),
        before_row: None,
        ..position()
    };
    assert!(
        owner
            .issue_cursor(unvisited_issue("search-a", unvisited))
            .is_ok()
    );
    let row_without_epoch = GlobalSearchSessionPosition {
        grid_epoch: String::new(),
        ..position()
    };
    let refused = owner.issue_cursor(unvisited_issue("search-a", row_without_epoch));
    assert_eq!(refused, Err(CursorIssueRefusal::RowWithoutEpoch));
    assert!(
        refused
            .unwrap_err()
            .to_string()
            .contains("row continuation requires a grid epoch")
    );
    let repeated = owner.issue_cursor(issue(
        binding("search-a"),
        vec![(position(), false, None), (position(), false, None)],
        2,
        Vec::new(),
    ));
    assert!(repeated.unwrap_err().to_string().contains("must be unique"));
}

// "requires a searched session to resume strictly older than the row it was given"
#[test]
fn a_searched_session_must_resume_strictly_older_than_its_requested_row() {
    let owner = GlobalSearchCursorOwner::new();
    let requested = position().before_row;
    let attempt = |resumed: GlobalSearchSessionPosition, searched: bool| {
        owner.issue_cursor(issue(
            binding("search-a"),
            vec![(resumed, searched, requested)],
            1,
            vec![position().session_id],
        ))
    };
    let requested_row = requested.unwrap();
    assert!(
        attempt(
            GlobalSearchSessionPosition {
                before_row: Some(requested_row - 1),
                ..position()
            },
            true
        )
        .is_ok()
    );
    // An epoch reset restarts the session from its newest row: real progress.
    assert!(
        attempt(
            GlobalSearchSessionPosition {
                grid_epoch: String::new(),
                before_row: None,
                ..position()
            },
            true
        )
        .is_ok()
    );
    // A page that never reached the session may retry the same position.
    assert!(attempt(position(), false).is_ok());
    for before_row in [requested_row, requested_row + 1] {
        let refused = attempt(
            GlobalSearchSessionPosition {
                before_row: Some(before_row),
                ..position()
            },
            true,
        );
        assert!(
            refused
                .unwrap_err()
                .to_string()
                .contains("must advance a searched session")
        );
    }
}

// "keeps an eligible count larger than one page"
#[test]
fn the_eligible_count_may_exceed_one_page_but_never_undercount_it() {
    let owner = GlobalSearchCursorOwner::new();
    let eligible = LIMITS.max_sessions * 4;
    let cursor = owner
        .issue_cursor(issue(
            binding("search-a"),
            vec![(position(), false, None)],
            eligible,
            vec![position().session_id],
        ))
        .unwrap();
    assert_eq!(
        owner
            .claim_cursor(&cursor, &binding("search-a"))
            .map(|p| p.eligible_sessions),
        Some(eligible)
    );
    let refused = owner.issue_cursor(issue(
        binding("search-a"),
        vec![(position(), false, None)],
        0,
        Vec::new(),
    ));
    assert!(
        refused
            .unwrap_err()
            .to_string()
            .contains("requires bounded progress")
    );
}

// "bounds active searches and cancellation tombstones per device"
#[test]
fn active_searches_and_cancel_tombstones_are_bounded_per_device() {
    let owner = GlobalSearchCursorOwner::new();
    for index in 0..GLOBAL_SEARCH_MAX_ACTIVE_PER_DEVICE {
        assert_eq!(
            owner.begin_search(&identity(&format!("active-{index}"))),
            GlobalSearchAdmission::Started
        );
    }
    assert_eq!(
        owner.begin_search(&identity("active-overflow")),
        GlobalSearchAdmission::Capacity
    );

    let tombstones = GlobalSearchCursorOwner::new();
    for index in 0..=GLOBAL_SEARCH_MAX_CANCEL_TOMBSTONES_PER_DEVICE {
        tombstones.prepare_cancellation(&identity(&format!("cancel-{index}")));
    }
    assert_eq!(
        tombstones.begin_search(&identity("cancel-0")),
        GlobalSearchAdmission::Started
    );
    let newest = format!("cancel-{GLOBAL_SEARCH_MAX_CANCEL_TOMBSTONES_PER_DEVICE}");
    assert_eq!(
        tombstones.begin_search(&identity(&newest)),
        GlobalSearchAdmission::Cancelled
    );
}
