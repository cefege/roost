//! The boundaries a direct-terminal credential minted on the wrong side of
//! must not cross: an answer that arrives after its auth generation was
//! replaced, or after the worker was retired.
//! The scope half of the lifecycle is in `local_terminal_grant_scope` and the
//! backoff in `local_terminal_grant_backoff`; the shared session table and the
//! mint-completing harness live in `local_terminal_grants_support`.

mod local_terminal_grants_support;

use roost_client_core::client::local::GrantRefresh;

use local_terminal_grants_support::{Harness, open_mint};

#[test]
fn does_not_install_a_grant_minted_before_an_auth_boundary() {
    let mut harness = Harness::new();
    let request = open_mint(&mut harness);
    harness.owner.set_auth_generation(1);
    let answer = harness.answer_for(&request);

    assert_eq!(
        harness
            .owner
            .complete_mint(&request, Ok(answer), &harness.sessions, 0),
        GrantRefresh::Discarded,
        "a credential minted for a replaced identity is never installed"
    );
    assert_eq!(harness.owner.current("worker-a"), None);
}

#[test]
fn removal_fences_an_in_flight_mint_and_a_previously_valid_grant() {
    let mut harness = Harness::new();
    let request = open_mint(&mut harness);
    harness.owner.retire_worker("worker-a");
    let answer = harness.answer_for(&request);

    assert_eq!(
        harness
            .owner
            .complete_mint(&request, Ok(answer), &harness.sessions, 0),
        GrantRefresh::Discarded,
        "a retirement fences an answer that had not arrived yet"
    );
    assert_eq!(harness.owner.current("worker-a"), None);
    let published = harness.owner.take_publications();
    assert_eq!(
        published.len(),
        1,
        "one publication: the retirement, not the answer"
    );
    assert_eq!(
        published[0].grant, None,
        "consumers are told the grant is GONE"
    );

    assert!(harness.owner.is_worker_retired("worker-a"));

    // The other half: a previously VALID grant dies on the same retirement.
    let mut valid = Harness::new();
    valid.demand("worker-b", "session-b", true);
    assert!(valid.owner.current("worker-b").is_some());
    valid.owner.retire_worker("worker-b");
    assert_eq!(valid.owner.current("worker-b"), None);
    let asked = valid.requests.len();
    assert_eq!(
        valid.renew("worker-b"),
        GrantRefresh::Standing(None),
        "a retired worker mints nothing again, and the answer is no grant"
    );
    assert_eq!(valid.requests.len(), asked, "the renewal asked for nothing");
}

#[test]
fn retiring_an_absent_worker_blocks_new_demand_until_auth_reset_while_another_worker_remains_usable()
 {
    let mut harness = Harness::new();
    harness.owner.retire_worker("worker-a");
    let blocked = harness.demand("worker-a", "session-a", true);
    assert_eq!(
        blocked,
        GrantRefresh::Standing(None),
        "a worker retired before it was ever seen still blocks new demand"
    );
    assert!(harness.requests.is_empty(), "and produces no mint at all");

    harness.demand("worker-b", "session-b", true);
    assert_eq!(
        harness.requested("worker-b"),
        vec![vec!["session-b".to_string()]],
        "losing one worker must not cost another one its terminal"
    );

    harness.owner.reset();
    harness.demand("worker-a", "session-a", true);
    assert_eq!(
        harness.requests.last().map(|r| r.worker_fp.as_str()),
        Some("worker-a"),
        "the auth boundary reset clears the retirement and mints again"
    );
}
