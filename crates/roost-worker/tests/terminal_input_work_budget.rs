//! The browser input work budget: worker-wide and per-port input caps, route
//! claim caps, and release exactly once by drop. Ports
//! `apps/worker/tests/terminal/terminal-input-work-budget.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_worker::terminal_input::work_budget::{
    INPUT_ADMISSION_FULL, InputWorkOrigin, ROUTE_CLAIM_BUSY, TERMINAL_DIRECT_INPUT_WORK_MAX_BYTES,
    TERMINAL_DIRECT_INPUT_WORK_MAX_REQUESTS, TERMINAL_INPUT_WORK_MAX_BYTES,
    TERMINAL_INPUT_WORK_MAX_REQUESTS, TERMINAL_ROUTE_CLAIM_WORK_MAX_REQUESTS,
    TerminalInputWorkBudget,
};

fn direct(port_id: &str) -> InputWorkOrigin {
    InputWorkOrigin::Direct {
        port_id: port_id.to_owned(),
    }
}

#[test]
fn global_input_count_and_bytes_are_held_until_the_reservation_drops() {
    let budget = TerminalInputWorkBudget::new();
    let mut held: Vec<_> = (0..TERMINAL_INPUT_WORK_MAX_REQUESTS)
        .map(|_| {
            budget
                .reserve_input(&InputWorkOrigin::Sync, 1)
                .expect("under the cap")
        })
        .collect();
    assert_eq!(
        budget.reserve_input(&InputWorkOrigin::Sync, 1).err(),
        Some(INPUT_ADMISSION_FULL)
    );
    held.pop();
    assert!(
        budget.reserve_input(&InputWorkOrigin::Sync, 1).is_ok(),
        "a dropped reservation frees its slot"
    );
    drop(held);

    let bytes = TerminalInputWorkBudget::new();
    let full = bytes
        .reserve_input(&InputWorkOrigin::Sync, TERMINAL_INPUT_WORK_MAX_BYTES)
        .unwrap();
    assert!(bytes.reserve_input(&InputWorkOrigin::Sync, 1).is_err());
    drop(full);
    let again = bytes.reserve_input(&InputWorkOrigin::Sync, TERMINAL_INPUT_WORK_MAX_BYTES);
    assert!(again.is_ok(), "released bytes are reusable exactly once");
    assert!(
        bytes.reserve_input(&InputWorkOrigin::Sync, 1).is_err(),
        "and not twice"
    );
}

#[test]
fn a_direct_port_is_capped_on_its_own_while_sync_spends_only_the_worker_ceiling() {
    let budget = TerminalInputWorkBudget::new();
    let held: Vec<_> = (0..TERMINAL_DIRECT_INPUT_WORK_MAX_REQUESTS)
        .map(|_| {
            budget
                .reserve_input(&direct("port-a"), 1)
                .expect("under the port cap")
        })
        .collect();
    assert!(budget.reserve_input(&direct("port-a"), 1).is_err());
    assert!(budget.reserve_input(&InputWorkOrigin::Sync, 1).is_ok());
    assert!(
        budget.reserve_input(&direct(""), 1).is_err(),
        "a direct batch names its port"
    );
    drop(held);
    assert!(budget.reserve_input(&direct("port-a"), 1).is_ok());

    let bytes = TerminalInputWorkBudget::new();
    let _full = bytes
        .reserve_input(&direct("port-b"), TERMINAL_DIRECT_INPUT_WORK_MAX_BYTES)
        .unwrap();
    assert!(bytes.reserve_input(&direct("port-b"), 1).is_err());
}

#[test]
fn route_claims_are_bounded_by_port_worker_and_actor_session() {
    let budget = TerminalInputWorkBudget::new();
    let held: Vec<_> = (0..4)
        .map(|index| {
            budget
                .reserve_route_claim("port-a", &format!("actor-session-{index}"))
                .unwrap()
        })
        .collect();
    assert_eq!(
        budget
            .reserve_route_claim("port-a", "actor-session-4")
            .err(),
        Some(ROUTE_CLAIM_BUSY)
    );
    assert_eq!(
        budget
            .reserve_route_claim("port-b", "actor-session-0")
            .err(),
        Some(ROUTE_CLAIM_BUSY),
        "one actor/session holds one claim"
    );
    drop(held);
    assert!(
        budget
            .reserve_route_claim("port-b", "actor-session-0")
            .is_ok()
    );

    let worker = TerminalInputWorkBudget::new();
    let _held: Vec<_> = (0..TERMINAL_ROUTE_CLAIM_WORK_MAX_REQUESTS)
        .map(|index| {
            worker
                .reserve_route_claim(&format!("port-{}", index / 4), &format!("key-{index}"))
                .unwrap()
        })
        .collect();
    assert!(
        worker
            .reserve_route_claim("port-extra", "key-extra")
            .is_err()
    );
}

#[test]
fn a_disposed_budget_refuses_and_late_drops_return_nothing() {
    let budget = TerminalInputWorkBudget::new();
    let early = budget.reserve_input(&InputWorkOrigin::Sync, 1).unwrap();
    budget.dispose();
    drop(early);
    assert!(budget.reserve_input(&InputWorkOrigin::Sync, 1).is_err());
    assert!(budget.reserve_route_claim("port", "key").is_err());
}
