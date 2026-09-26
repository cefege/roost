//! The retained tab reports: the TTL, the two cardinality caps, and the
//! new-identity budget's seam.
//!
//! The TTL and the caps are the two facts a UI surface can be broken by and
//! nothing else notices: a dead tab that never expires shows every other
//! browser a pane that is not there, and a cap that is per-fingerprint but not
//! aggregate lets a fleet of authorized devices multiply the retention without
//! bound. Both are asserted here against the numbers themselves, not against a
//! copy of them.

// A test that cannot say what it expected is not a test. `expect` is denied
// outside `#[cfg(test)]`, and an integration test is its own crate, so the
// exemption has to be stated here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod ui_state_fixture;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use roost_coord::ui_state::identity_rate::IdentityRateLimiter;
use roost_coord::ui_state::limits::{
    UI_STATE_MAX_TABS_PER_FINGERPRINT, UI_STATE_MAX_TABS_TOTAL, UI_STATE_NEW_IDENTITIES_PER_WINDOW,
};
use roost_coord::ui_state::state_owner::{UI_STATE_TTL_MS, UiStateOwner, UiStateReportError};
use ui_state_fixture::report_request;

/// A limiter that admits a fixed number of calls and then refuses.
struct BudgetedLimiter {
    remaining: AtomicUsize,
}

impl IdentityRateLimiter for BudgetedLimiter {
    fn consume(
        &self,
        _scope: &str,
        _group: &str,
        _capacity: usize,
        _window_ms: i64,
        _now_ms: i64,
    ) -> bool {
        self.remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |held| {
                held.checked_sub(1)
            })
            .is_ok()
    }
}

fn owner_with_now(now_ms: Arc<Mutex<i64>>) -> UiStateOwner {
    let clock = Arc::new(move || *now_ms.lock().expect("the test clock"));
    UiStateOwner::with_clock(clock)
}

#[test]
fn a_report_survives_exactly_the_ttl_and_is_gone_one_millisecond_past_it() {
    let now = Arc::new(Mutex::new(0_i64));
    let owner = owner_with_now(Arc::clone(&now));

    owner
        .report(
            "fingerprint-a",
            "tab-1",
            report_request("tab-1", "/s/one", None),
        )
        .expect("a first report is admitted");

    *now.lock().expect("the test clock") += UI_STATE_TTL_MS;
    assert_eq!(
        owner.list().len(),
        1,
        "a report is still retained at exactly the TTL"
    );

    *now.lock().expect("the test clock") += 1;
    assert!(
        owner.list().is_empty(),
        "a report one millisecond past the TTL is gone"
    );
    assert_eq!(owner.retained_count(), 0);
}

#[test]
fn the_ttl_and_both_caps_are_the_numbers_the_contract_states() {
    assert_eq!(UI_STATE_TTL_MS, 5 * 60_000);
    assert_eq!(UI_STATE_MAX_TABS_TOTAL, 256);
    assert_eq!(UI_STATE_MAX_TABS_PER_FINGERPRINT, 32);
    assert_eq!(UI_STATE_NEW_IDENTITIES_PER_WINDOW, 16);
}

#[test]
fn a_heartbeat_refreshes_an_existing_tab_without_consuming_an_identity() {
    let now = Arc::new(Mutex::new(0_i64));
    let owner = owner_with_now(Arc::clone(&now)).with_identity_rate_limiter(Arc::new(
        BudgetedLimiter {
            remaining: AtomicUsize::new(1),
        },
    ));

    owner
        .report(
            "fingerprint-a",
            "tab-1",
            report_request("tab-1", "/s/old", None),
        )
        .expect("the first report spends the identity budget");
    *now.lock().expect("the test clock") += 1_000;
    owner
        .report(
            "fingerprint-a",
            "tab-1",
            report_request("tab-1", "/s/new", None),
        )
        .expect("an existing tab heartbeats without spending a new identity");
    assert_eq!(owner.retained_count(), 1);
    assert_eq!(owner.list()[0].state.active_path, "/s/new");

    assert_eq!(
        owner.report(
            "fingerprint-a",
            "tab-2",
            report_request("tab-2", "/s/other", None)
        ),
        Err(UiStateReportError::IdentityRate)
    );
    assert_eq!(
        owner.retained_count(),
        1,
        "a refused identity retained nothing"
    );
}

#[test]
fn one_device_is_capped_and_the_aggregate_cap_is_reached_across_devices() {
    let now = Arc::new(Mutex::new(0_i64));
    let owner = owner_with_now(Arc::clone(&now));

    let per_device = UI_STATE_MAX_TABS_PER_FINGERPRINT;

    for index in 0..per_device {
        owner
            .report(
                "fingerprint-0",
                &format!("tab-{index}"),
                report_request(&format!("tab-{index}"), "/", None),
            )
            .expect("a tab below the per-fingerprint cap");
    }
    assert_eq!(
        owner.report(
            "fingerprint-0",
            "tab-over",
            report_request("tab-over", "/", None)
        ),
        Err(UiStateReportError::Capacity)
    );
    assert_eq!(owner.retained_count(), per_device);

    // Every further device is at its own cap too, so the aggregate cap is only
    // reachable by filling devices -- which is why there are two of them.
    for device in 1..(UI_STATE_MAX_TABS_TOTAL / per_device) {
        for index in 0..per_device {
            owner
                .report(
                    &format!("fingerprint-{device}"),
                    &format!("tab-{device}-{index}"),
                    report_request(&format!("tab-{device}-{index}"), "/", None),
                )
                .expect("a tab below both caps");
        }
    }
    assert_eq!(owner.retained_count(), UI_STATE_MAX_TABS_TOTAL);
    assert_eq!(
        owner.report(
            "fingerprint-new",
            "tab-over",
            report_request("tab-over", "/", None)
        ),
        Err(UiStateReportError::Capacity),
        "a device nobody has made room for is refused on the aggregate cap"
    );
    assert_eq!(owner.retained_count(), UI_STATE_MAX_TABS_TOTAL);
}
