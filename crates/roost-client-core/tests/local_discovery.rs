//! Which origin this page talks to, and which worker's door it can reach.
//!
//! Ported from `apps/web/tests/client/carriers/localBootstrap.test.ts` and
//! `.../localWorkerDiscovery.test.ts`. The v2 tests stub `fetch`, `location` and
//! `localStorage` and assert on the requests that came out; this port describes
//! the same environment as a value and asserts on the decision, which is the
//! part that was ever in doubt — a browser here would only be proving that the
//! stub answered.
//!
//! The two answers are deliberately different questions. `bootstrap` decides
//! where coordinator RPCs GO, and an unusable answer must leave the page on its
//! own origin. `discovery` decides which worker's door to dial, and an unusable
//! answer must leave the page with no door at all. Neither may be satisfied by
//! the other, which is why the malformed-override cases are in the second file
//! and the unusable-body cases in the first.

use roost_client_core::client::local::LocalWorkerDoor;
use roost_client_core::client::local::bootstrap::{
    BootstrapOutcome, BootstrapRefusal, LocalBootstrap, coordinator_base, coordinator_base_url,
    read_serving_origin,
};
use roost_client_core::client::local::discovery::{
    BrowserEnvironment, DEFAULT_WORKER_LOCAL_UI_ORIGIN, DoorAbsence, DoorAdoption, DoorDiscovery,
    DoorPlan, LOCAL_WORKER_ORIGIN_KEY, candidate_origin, origin_is_loopback,
};
use roost_client_core::client::local::door::{
    DialRefusal, local_terminal_url, redial_ceiling_ms, redial_delay_ms,
};
use roost_client_core::client::local::outbound::{RouteClaims, SyncTerminalState};

const PAGE_ORIGIN: &str = "https://mic.roost.test";

/// v2's `DOOR_ANSWER`: a coordinator this door does not serve, and the worker
/// fingerprint the door DOES serve.
const DOOR_ANSWER: &str =
    r#"{"coordinatorUrl":"https://coord.other.test:4102","workerFingerprint":"fp-local-worker"}"#;

/// A page the coordinator served, which is the case that probes.
fn coordinator_served_page(operator_origin: Option<&str>) -> BrowserEnvironment {
    BrowserEnvironment {
        page_origin: PAGE_ORIGIN.to_string(),
        served_by_worker: None,
        operator_origin: operator_origin.map(str::to_string),
    }
}

/// A page a worker served, which knows its door without asking anything.
fn worker_served_page(worker_fingerprint: &str) -> BrowserEnvironment {
    BrowserEnvironment {
        page_origin: PAGE_ORIGIN.to_string(),
        served_by_worker: Some(LocalBootstrap {
            coordinator_url: "https://coord.example:4102".to_string(),
            worker_fingerprint: worker_fingerprint.to_string(),
        }),
        operator_origin: None,
    }
}

#[test]
fn a_worker_served_page_adopts_the_advertised_coordinator() {
    let outcome = read_serving_origin(Some(200), DOOR_ANSWER);
    let BootstrapOutcome::Served(bootstrap) = outcome else {
        panic!("a 200 with a usable body is a served page");
    };
    assert_eq!(bootstrap.coordinator_url, "https://coord.other.test:4102");
    assert_eq!(bootstrap.worker_fingerprint, "fp-local-worker");
    assert_eq!(
        coordinator_base(Some(&bootstrap), None, None),
        "https://coord.other.test:4102",
        "the worker that served the page is the only one that can say where its \
         coordinator is"
    );
    assert_eq!(
        coordinator_base_url("https://coord.other.test:4102", PAGE_ORIGIN),
        "https://coord.other.test:4102"
    );
}

#[test]
fn a_coordinator_served_page_keeps_its_own_origin() {
    assert_eq!(
        read_serving_origin(Some(404), "Not Found"),
        BootstrapOutcome::NotWorkerServed(BootstrapRefusal::Status)
    );
    assert_eq!(coordinator_base(None, None, None), "");
    assert_eq!(
        coordinator_base_url("", PAGE_ORIGIN),
        PAGE_ORIGIN,
        "with no advertised coordinator the page dials its own origin"
    );
}

#[test]
fn an_unusable_answer_never_retargets_the_spa() {
    let bodies = [
        "<!doctype html><title>spa</title>",
        "{",
        r#"{"coordinatorUrl":"https://coord.example"}"#,
        r#"{"workerFingerprint":"fp-local-worker"}"#,
        r#"{"coordinatorUrl":"  ","workerFingerprint":"fp-local-worker"}"#,
        r#"{"coordinatorUrl":"/api","workerFingerprint":"fp-local-worker"}"#,
        r#"{"coordinatorUrl":4102,"workerFingerprint":"fp-local-worker"}"#,
        r#"["https://coord.example"]"#,
        "null",
    ];
    for body in bodies {
        let outcome = read_serving_origin(Some(200), body);
        assert!(
            matches!(outcome, BootstrapOutcome::NotWorkerServed(_)),
            "{body:?} is not a bootstrap and must leave the page on its own path"
        );
        assert_eq!(coordinator_base(None, None, None), "");
        assert_eq!(coordinator_base_url("", PAGE_ORIGIN), PAGE_ORIGIN);
    }
    assert_eq!(
        read_serving_origin(
            Some(200),
            r#"{"coordinatorUrl":"ftp://coord","workerFingerprint":"f"}"#
        ),
        BootstrapOutcome::NotWorkerServed(BootstrapRefusal::NotHttpUrl),
        "a non-HTTP coordinator URL would be resolved against this page's own \
         origin, which is the worker that served it"
    );
}

#[test]
fn a_rejected_probe_is_not_a_startup_failure() {
    assert_eq!(
        read_serving_origin(None, ""),
        BootstrapOutcome::NotWorkerServed(BootstrapRefusal::Unreachable),
        "a request that never completed is its own answer, distinct from a \
         status the origin disliked"
    );
    assert_eq!(coordinator_base(None, None, None), "");
}

#[test]
fn a_worker_served_page_adopts_its_own_origin_without_probing() {
    let mut discovery = DoorDiscovery::new();
    let plan = discovery.start(&worker_served_page("fp-serving-worker"));

    assert_eq!(
        plan,
        DoorPlan::Adopting(LocalWorkerDoor {
            origin: PAGE_ORIGIN.to_string(),
            worker_fingerprint: "fp-serving-worker".to_string(),
        }),
        "a page a worker served knows the door from its own origin"
    );
    assert_eq!(
        discovery.door().map(|door| door.origin.as_str()),
        Some(PAGE_ORIGIN)
    );
}

#[test]
fn a_coordinator_served_page_probes_the_default_loopback_door() {
    let mut discovery = DoorDiscovery::new();
    let plan = discovery.start(&coordinator_served_page(None));

    assert_eq!(
        plan,
        DoorPlan::Probe {
            origin: DEFAULT_WORKER_LOCAL_UI_ORIGIN.to_string(),
            url: format!("{DEFAULT_WORKER_LOCAL_UI_ORIGIN}/api/local-bootstrap"),
        }
    );
    assert_eq!(
        discovery.complete_probe(DEFAULT_WORKER_LOCAL_UI_ORIGIN, Some(200), DOOR_ANSWER),
        DoorAdoption::Adopted(LocalWorkerDoor {
            origin: DEFAULT_WORKER_LOCAL_UI_ORIGIN.to_string(),
            worker_fingerprint: "fp-local-worker".to_string(),
        })
    );
}

#[test]
fn a_valid_override_redirects_the_probe_and_a_malformed_one_is_ignored() {
    let mut discovery = DoorDiscovery::new();
    assert_eq!(
        discovery.start(&coordinator_served_page(Some("http://127.0.0.1:9999"))),
        DoorPlan::Probe {
            origin: "http://127.0.0.1:9999".to_string(),
            url: "http://127.0.0.1:9999/api/local-bootstrap".to_string(),
        },
        "an operator who moved the port gets that port probed"
    );

    for malformed in [
        "127.0.0.1:9999",
        "http://127.0.0.1:9999/",
        "not a url",
        "ws://127.0.0.1:9999",
    ] {
        assert_eq!(
            candidate_origin(Some(malformed)),
            DEFAULT_WORKER_LOCAL_UI_ORIGIN,
            "{malformed:?} is not a bare http origin, so it is ignored rather \
             than dialled"
        );
    }
    assert_eq!(LOCAL_WORKER_ORIGIN_KEY, "roost.localWorkerOrigin");
}

#[test]
fn an_unreachable_or_unusable_door_leaves_the_page_with_none() {
    let refusals: [(Option<u16>, &str); 5] = [
        (Some(404), "Not Found"),
        (Some(200), "{"),
        (Some(200), r#"{"coordinatorUrl":"https://coord.test"}"#),
        (
            Some(200),
            r#"{"coordinatorUrl":"https://coord.test","workerFingerprint":"  "}"#,
        ),
        (None, ""),
    ];
    for (status, body) in refusals {
        let mut discovery = DoorDiscovery::new();
        discovery.start(&coordinator_served_page(None));
        let adoption = discovery.complete_probe(DEFAULT_WORKER_LOCAL_UI_ORIGIN, status, body);
        assert!(
            matches!(adoption, DoorAdoption::Absent(_)),
            "status {status:?} body {body:?} must not produce a door"
        );
        assert_eq!(
            discovery.door(),
            None,
            "an absent door leaves none recorded"
        );
        assert!(discovery.take_adoptions().is_empty());
    }
}

/// The rule the two v2 discovery files together exist to protect: a worker is
/// adopted ONLY when an origin this browser actually asked answered for it.
#[test]
fn discovery_refuses_a_worker_that_is_not_reachable_from_the_browser() {
    // The page is the candidate: the serving origin already answered 404 for this
    // path during startup, so asking again is a request that cannot help.
    let mut same_origin = DoorDiscovery::new();
    assert_eq!(
        same_origin.start(&BrowserEnvironment {
            page_origin: DEFAULT_WORKER_LOCAL_UI_ORIGIN.to_string(),
            served_by_worker: None,
            operator_origin: None,
        }),
        DoorPlan::NotProbed(DoorAbsence::SameOrigin {
            origin: DEFAULT_WORKER_LOCAL_UI_ORIGIN.to_string()
        }),
        "a page served BY the default door has already been told there is no \
         bootstrap there; it must not conclude it has a door"
    );
    assert_eq!(same_origin.door(), None);

    // A probe that answers for an origin this page never asked is stale, and a
    // stale answer must not install a door even if its body is perfect.
    let mut discovery = DoorDiscovery::new();
    discovery.start(&coordinator_served_page(None));
    assert_eq!(
        discovery.complete_probe("http://127.0.0.1:9999", Some(200), DOOR_ANSWER),
        DoorAdoption::Absent(DoorAbsence::StaleProbe {
            origin: "http://127.0.0.1:9999".to_string()
        }),
        "an answer from an origin this page did not probe says nothing about the \
         door it is waiting on"
    );
    assert_eq!(discovery.door(), None);
    assert!(discovery.take_adoptions().is_empty());

    // And the worker named in a body is adopted only at the origin that was
    // ASKED — never at one the body could have chosen.
    let mut reached = DoorDiscovery::new();
    reached.start(&coordinator_served_page(Some("http://127.0.0.1:9999")));
    let DoorAdoption::Adopted(door) = reached.complete_probe(
        "http://127.0.0.1:9999",
        Some(200),
        r#"{"coordinatorUrl":"https://coord.test","workerFingerprint":"fp-local-worker"}"#,
    ) else {
        panic!("the origin that answered is reachable, so its door is adopted");
    };
    assert_eq!(door.origin, "http://127.0.0.1:9999");
    assert_eq!(door.worker_fingerprint, "fp-local-worker");
}

#[test]
fn discovery_runs_once_per_page_and_notifies_its_handler_on_adoption() {
    let mut discovery = DoorDiscovery::new();
    discovery.start(&coordinator_served_page(None));
    discovery.complete_probe(DEFAULT_WORKER_LOCAL_UI_ORIGIN, Some(200), DOOR_ANSWER);

    // The second call must ask nothing, and the host hears about the door once.
    assert_eq!(
        discovery.start(&coordinator_served_page(Some("http://127.0.0.1:9999"))),
        DoorPlan::AlreadyAttempted,
        "the caller is on a terminal pane's publish path and must not wait on a \
         second probe"
    );
    let adopted = discovery.take_adoptions();
    assert_eq!(adopted.len(), 1);
    assert_eq!(adopted[0].worker_fingerprint, "fp-local-worker");
    assert!(
        discovery.take_adoptions().is_empty(),
        "a host that has read the adoption is not told twice"
    );
}

#[test]
fn an_origin_that_is_not_a_door_is_never_dialled() {
    assert_eq!(
        local_terminal_url("http://127.0.0.1:4114"),
        Ok("ws://127.0.0.1:4114/ws/local-terminal".to_string())
    );
    assert_eq!(
        local_terminal_url("https://worker.example"),
        Ok("wss://worker.example/ws/local-terminal".to_string()),
        "a TLS front door is reached as wss even when an operator wrote http"
    );
    for unusable in ["127.0.0.1:4114", "ws://127.0.0.1:4114", "not a url", ""] {
        assert!(
            matches!(
                local_terminal_url(unusable),
                Err(DialRefusal::UnusableOrigin { .. })
            ),
            "{unusable:?} is not a door origin and must not be dialled"
        );
    }
}

#[test]
fn the_redial_ladder_is_bounded_and_jittered() {
    assert_eq!(redial_ceiling_ms(0), 500);
    assert_eq!(redial_ceiling_ms(1), 1_000);
    assert_eq!(redial_ceiling_ms(4), 8_000);
    assert_eq!(
        redial_ceiling_ms(40),
        8_000,
        "a tab open for days must not wrap into a SHORT delay"
    );
    assert_eq!(
        redial_delay_ms(4, 0),
        4_000,
        "the sample's floor is the half-delay"
    );
    assert_eq!(
        redial_delay_ms(4, 999),
        7_996,
        "and its ceiling is under 8s"
    );
    assert!(
        (4_000..8_000).contains(&redial_delay_ms(4, 500)),
        "equal jitter keeps every delay in the upper half of the ladder, which is \
         what stops every open tab redialling a restarted worker at one instant"
    );
}

/// A claim's answer is matched on the coordinator's socket id AND the worker
/// process epoch, because an answer arriving on another connection is an answer
/// about a route this client no longer holds.
#[test]
fn a_claim_is_matched_on_the_connection_it_was_sent_on() {
    let state = |socket_id: &str, process_epoch: &str| SyncTerminalState {
        socket_generation: 7,
        socket_id: socket_id.to_string(),
        process_epoch: process_epoch.to_string(),
        domain_generation: 3,
        ready: true,
    };
    let mut claims = RouteClaims::new();
    assert_eq!(
        claims.admit("claim-1", &state("socket-a", "epoch-a"), 0),
        Ok(())
    );
    assert!(
        !claims.settle("claim-1", "socket-b", "epoch-a"),
        "an answer from another socket is about another route"
    );
    assert!(
        !claims.settle("claim-1", "socket-a", "epoch-b"),
        "an answer from another worker process is about another route"
    );
    assert_eq!(claims.outstanding(), 1, "neither wrong answer consumed it");
    assert!(claims.settle("claim-1", "socket-a", "epoch-a"));
    assert_eq!(claims.outstanding(), 0);
    assert!(
        claims.expire(0).is_empty(),
        "a claim inside its deadline stays"
    );
}

/// Only a loopback origin can have been served by a worker door; every other
/// page skips the serving-origin probe.
#[test]
fn only_a_loopback_page_origin_can_be_worker_served() {
    assert!(origin_is_loopback("http://127.0.0.1:4114"));
    assert!(origin_is_loopback("http://localhost"));
    assert!(origin_is_loopback("http://[::1]:4114"));
    assert!(!origin_is_loopback("https://mike.roosttt.com"));
    assert!(!origin_is_loopback("http://127.0.0.1.evil.example"));
    assert!(!origin_is_loopback(""));
}
