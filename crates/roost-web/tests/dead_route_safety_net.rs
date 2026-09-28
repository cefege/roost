//! The dead-route safety net: a blip back to a live session inside the grace
//! window never bounces; a durable miss bounces to a sibling in the folder or
//! home, with the reason v2 logs. Ports
//! `apps/web/tests/deadRouteSafetyNet.test.ts`; the host's timer is simulated
//! by holding the armed ticket and calling `fire` when it would expire.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::store::{
    ChannelId, Session, SessionId, SessionKind, SessionStatus, WorkerFp,
};
use roost_web::dead_route_safety_net::{DeadRouteSafetyNet, RouteLiveness, SafetyNetStep};

fn session(id: &str) -> Session {
    Session {
        id: SessionId::try_from(id.to_owned()).unwrap(),
        worker_fp: WorkerFp::try_from("aa".repeat(32)).unwrap(),
        channel: ChannelId::try_from(1_i64).unwrap(),
        kind: SessionKind::Shell,
        cwd: "/Users/you/roost".into(),
        spawn_cwd: Some("/Users/you/roost".into()),
        workspace_id: None,
        status: SessionStatus::Open,
        created_at: 1000,
        closed_at: None,
        custom_title: None,
        git_branch: None,
        git_remote: None,
        pr_number: None,
        pr_state: None,
        pr_checks: None,
        pr_url: None,
        ports: None,
    }
}

fn live(open: Option<&Session>) -> RouteLiveness<'_> {
    RouteLiveness {
        open_session: open,
        on_terminal_route: true,
        hydrated: true,
    }
}

fn armed(step: SafetyNetStep) -> u64 {
    match step {
        SafetyNetStep::Arm { ticket, grace_ms } => {
            assert_eq!(grace_ms, 2_500);
            ticket
        }
        SafetyNetStep::Idle => panic!("expected a grace timer"),
    }
}

#[test]
fn a_blip_that_recovers_inside_the_grace_window_never_bounces() {
    let viewed = session("00000000-0000-4000-8000-000000000001");
    let mut net = DeadRouteSafetyNet::default();
    assert_eq!(net.evaluate(live(Some(&viewed))), SafetyNetStep::Idle);
    let ticket = armed(net.evaluate(live(None)));
    assert_eq!(net.evaluate(live(Some(&viewed))), SafetyNetStep::Idle);
    assert_eq!(net.fire(ticket, false), None);
}

#[test]
fn a_recovery_seen_only_by_the_timers_recheck_cancels_the_bounce() {
    let viewed = session("00000000-0000-4000-8000-000000000001");
    let mut net = DeadRouteSafetyNet::default();
    net.evaluate(live(Some(&viewed)));
    let ticket = armed(net.evaluate(live(None)));
    assert_eq!(net.fire(ticket, true), None);
}

#[test]
fn a_durable_miss_after_a_live_session_bounces_as_gone() {
    let viewed = session("00000000-0000-4000-8000-000000000001");
    let mut net = DeadRouteSafetyNet::default();
    net.evaluate(live(Some(&viewed)));
    let ticket = armed(net.evaluate(live(None)));
    let bounce = net.fire(ticket, false).unwrap();
    assert_eq!(bounce.reason(), "gone");
    assert_eq!(bounce.last_open.unwrap().id.as_str(), viewed.id.as_str());
    assert_eq!(net.fire(ticket, false), None);
}

#[test]
fn nothing_arms_before_hydration() {
    let mut net = DeadRouteSafetyNet::default();
    let before = RouteLiveness {
        open_session: None,
        on_terminal_route: true,
        hydrated: false,
    };
    assert_eq!(net.evaluate(before), SafetyNetStep::Idle);
}

#[test]
fn a_deep_link_that_never_resolved_bounces_as_stale() {
    let mut net = DeadRouteSafetyNet::default();
    let ticket = armed(net.evaluate(live(None)));
    let bounce = net.fire(ticket, false).unwrap();
    assert_eq!(bounce.reason(), "stale-deeplink");
    assert!(bounce.last_open.is_none());
}

#[test]
fn off_a_terminal_route_nothing_arms() {
    let mut net = DeadRouteSafetyNet::default();
    let off = RouteLiveness {
        open_session: None,
        on_terminal_route: false,
        hydrated: true,
    };
    assert_eq!(net.evaluate(off), SafetyNetStep::Idle);
}

#[test]
fn a_re_evaluation_voids_the_earlier_timer_even_while_still_missing() {
    let mut net = DeadRouteSafetyNet::default();
    let first = armed(net.evaluate(live(None)));
    let second = armed(net.evaluate(live(None)));
    assert_eq!(net.fire(first, false), None);
    assert!(net.fire(second, false).is_some());
}
