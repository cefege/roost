//! Input-route fencing: exact-claim caching, stale revisions, epoch rotation,
//! claims queued behind the keeper lane, tombstones and device revocation.
//! Ports `apps/worker/tests/terminal/terminal-input-route-owner.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "session_support/mod.rs"]
mod session_support;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use roost_worker::session::input_write::TerminalWriteBudget;
use roost_worker::session::keeper_admission::{Admission, AdmissionKind, AdmissionTicket};
use roost_worker::terminal_input::route_owner::TERMINAL_INPUT_ROUTE_TOMBSTONE;
use roost_worker::terminal_input::work_budget::TERMINAL_ROUTE_CLAIM_WORK_MAX_REQUESTS;
use roost_worker::terminal_input::{
    RouteActor, RouteClaim, RouteClaimBudget, TerminalInputRouteOwner, TerminalInputWorkBudget,
};
use session_support::{Harness, SESSION, channel};

const EPOCH: &str = "worker-epoch";
const CHANNEL: u16 = 7;

struct Live(Arc<AtomicBool>);

impl TerminalWriteBudget for Live {
    fn is_current_connection(&self) -> bool {
        true
    }
    fn expired(&self) -> bool {
        false
    }
}

impl RouteClaimBudget for Live {
    fn is_session_authorized(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

fn actor(tag: &str) -> RouteActor {
    RouteActor {
        device_fingerprint: format!("device-{tag}"),
        tab_id: format!("tab-{tag}"),
        connection_id: format!("connection-{tag}"),
    }
}

fn claim(revision: u64, request_id: &str) -> RouteClaim {
    RouteClaim {
        request_id: request_id.to_owned(),
        session_id: SESSION.to_owned(),
        revision,
        worker_epoch: EPOCH.to_owned(),
    }
}

struct Fixture {
    harness: Harness,
    owner: TerminalInputRouteOwner,
    authorized: Arc<AtomicBool>,
    clock: Arc<Mutex<Instant>>,
}

fn fixture() -> Fixture {
    let harness = Harness::new();
    harness.install(SESSION, CHANNEL, "/home/user/project", "/home/user/project");
    let clock = Arc::new(Mutex::new(Instant::now()));
    let reading = Arc::clone(&clock);
    let owner = TerminalInputRouteOwner::with_clock(
        EPOCH.to_owned(),
        Arc::clone(&harness.table),
        Arc::clone(harness.manager.control_lanes()),
        TerminalInputWorkBudget::new(),
        Arc::new(move || *reading.lock().unwrap()),
    );
    Fixture {
        harness,
        owner,
        authorized: Arc::new(AtomicBool::new(true)),
        clock,
    }
}

impl Fixture {
    fn budget(&self) -> Box<Live> {
        Box::new(Live(Arc::clone(&self.authorized)))
    }

    async fn hold_lane(&self) -> AdmissionTicket {
        let Admission::Granted(ticket) = self
            .harness
            .manager
            .control_lanes()
            .admit(channel(i64::from(CHANNEL)), AdmissionKind::TerminalResize)
        else {
            panic!("the lane admits");
        };
        ticket.granted().await;
        ticket
    }
}

#[tokio::test]
async fn an_exact_claim_is_cached_a_stale_one_refused_and_a_newer_one_rotates_the_epoch() {
    let f = fixture();
    let first = f.owner.claim(actor("a"), claim(1, "one"), f.budget()).await;
    assert!(first.accepted && first.worker_epoch == EPOCH);
    assert!(
        f.owner
            .is_current(&actor("a"), SESSION, &first.input_route_epoch)
    );
    assert_eq!(
        f.owner.claim(actor("a"), claim(1, "one"), f.budget()).await,
        first
    );
    let stale = f
        .owner
        .claim(actor("a"), claim(1, "other"), f.budget())
        .await;
    assert_eq!(
        (stale.accepted, stale.latest_revision, stale.reason.as_str()),
        (false, 1, "stale_route_revision")
    );
    let next = f.owner.claim(actor("a"), claim(2, "two"), f.budget()).await;
    assert!(next.accepted && next.input_route_epoch != first.input_route_epoch);
    assert!(
        !f.owner
            .is_current(&actor("a"), SESSION, &first.input_route_epoch)
    );
    assert!(
        f.owner
            .is_current(&actor("a"), SESSION, &next.input_route_epoch)
    );
}

#[tokio::test]
async fn out_of_range_claims_are_refused_before_keeper_admission() {
    let f = fixture();
    for bad in [
        claim(0, "zero"),
        claim(1 << 63, "too-large"),
        claim(1, &"x".repeat(129)),
    ] {
        assert_eq!(
            f.owner.claim(actor("a"), bad, f.budget()).await.reason,
            "invalid_route_claim"
        );
    }
    f.authorized.store(false, Ordering::SeqCst);
    assert_eq!(
        f.owner
            .claim(actor("a"), claim(1, "gone"), f.budget())
            .await
            .reason,
        "terminal session is unavailable"
    );
}

#[tokio::test]
async fn a_queued_claim_that_loses_authority_never_restores_the_prior_epoch_and_blocks_newer_claims()
 {
    let f = fixture();
    let initial = f
        .owner
        .claim(actor("a"), claim(1, "before"), f.budget())
        .await;
    let blocker = f.hold_lane().await;
    let replacement = f.owner.claim(actor("a"), claim(2, "held"), f.budget());
    assert!(
        !f.owner
            .is_current(&actor("a"), SESSION, &initial.input_route_epoch),
        "the old epoch stops at the claim"
    );
    let busy = f
        .owner
        .claim(actor("a"), claim(3, "refused"), f.budget())
        .await;
    assert_eq!(busy.reason, "route_claim_busy");
    f.authorized.store(false, Ordering::SeqCst);
    blocker.release();
    assert_eq!(replacement.await.reason, "terminal session is unavailable");
    assert!(
        !f.owner
            .is_current(&actor("a"), SESSION, &initial.input_route_epoch)
    );
}

/// Retired claims keep their capacity charged until their lane tickets drain,
/// so retiring cannot be used to pile unbounded tickets behind a held lane.
#[tokio::test]
async fn retired_claims_keep_their_capacity_until_their_tickets_drain() {
    let f = fixture();
    let blocker = f.hold_lane().await;
    let mut retired = Vec::new();
    for index in 0..TERMINAL_ROUTE_CLAIM_WORK_MAX_REQUESTS {
        let who = actor(&format!("retired-{index}"));
        retired.push(tokio::spawn(f.owner.claim(
            who.clone(),
            claim(1, &format!("r{index}")),
            f.budget(),
        )));
        f.owner.retire_connection(&who.connection_id);
    }
    tokio::task::yield_now().await;
    let overflow = f
        .owner
        .claim(actor("overflow"), claim(1, "overflow"), f.budget())
        .await;
    assert_eq!(overflow.reason, "route_claim_busy");
    blocker.release();
    for claim in retired {
        assert_eq!(claim.await.unwrap().reason, "route_retired");
    }
    let drained = AtomicU64::new(0);
    for _ in 0..200 {
        tokio::time::sleep(Duration::from_millis(5)).await;
        if f.owner
            .claim(actor("after"), claim(1, "after"), f.budget())
            .await
            .accepted
        {
            drained.store(1, Ordering::SeqCst);
            break;
        }
    }
    assert_eq!(
        drained.load(Ordering::SeqCst),
        1,
        "drained tickets return their capacity"
    );
}

#[tokio::test]
async fn a_tombstone_fences_its_revision_until_its_lease_ends_and_revocation_fences_the_device() {
    let f = fixture();
    let active = f
        .owner
        .claim(actor("a"), claim(9, "tomb"), f.budget())
        .await;
    f.owner.retire_connection(&actor("a").connection_id);
    assert!(
        !f.owner
            .is_current(&actor("a"), SESSION, &active.input_route_epoch)
    );
    assert!(!f.owner.allows_legacy_input(&actor("a"), SESSION));
    let stale = f
        .owner
        .claim(actor("a"), claim(9, "tomb-other"), f.budget())
        .await;
    assert_eq!(
        (stale.latest_revision, stale.reason.as_str()),
        (9, "stale_route_revision")
    );
    *f.clock.lock().unwrap() += TERMINAL_INPUT_ROUTE_TOMBSTONE + Duration::from_millis(1);
    assert!(f.owner.allows_legacy_input(&actor("a"), SESSION));
    let after = f
        .owner
        .claim(actor("a"), claim(1, "after-prune"), f.budget())
        .await;
    assert!(after.accepted && after.revision == 1);
    f.owner.revoke_device(&actor("a").device_fingerprint);
    assert!(
        !f.owner
            .is_current(&actor("a"), SESSION, &after.input_route_epoch)
    );
    let other_tab = RouteActor {
        tab_id: "tab-other".to_owned(),
        ..actor("a")
    };
    assert!(!f.owner.allows_legacy_input(&other_tab, SESSION));
}
