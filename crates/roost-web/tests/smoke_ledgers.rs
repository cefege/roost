//! The smoke backdoor's ledgers over fakes: scoped cleanup, the
//! created-resource ledger, the timing clocks and the DOM-hold refusals. Pins
//! `smoke::{created_resources, timing, dom_hold}` (v2 `smokeCreatedResources.ts`,
//! `smokeHarness.ts` timings, `smokeTerminalDomFault.ts`).
#![cfg(feature = "smoke")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use roost_client_core::TerminalToken;
use roost_web::smoke::call::TimingKind;
use roost_web::smoke::created_resources::{
    CleanupRpc, CreatedResources, cleanup_created, free_workspace_name,
};
use roost_web::smoke::dom_hold::DomHolds;
use roost_web::smoke::timing::{TIMING_CAPACITY, TimingLedger, timing_result};
use serde_json::json;

fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
    }
}

#[derive(Default)]
struct FakeCoordinator {
    kill_refusals: Vec<&'static str>,
    versions: RefCell<BTreeMap<String, u64>>,
    delete_failures: Cell<u32>,
    deletes: RefCell<Vec<(String, u64)>>,
}

impl CleanupRpc for FakeCoordinator {
    async fn kill_session(&self, session_id: &str) -> Result<(), String> {
        if self.kill_refusals.contains(&session_id) {
            Err("NotFound: gone".into())
        } else {
            Ok(())
        }
    }
    async fn workspace_versions(&self) -> Result<BTreeMap<String, u64>, String> {
        Ok(self.versions.borrow().clone())
    }
    async fn delete_workspace(&self, workspace_id: &str, version: u64) -> Result<(), String> {
        self.deletes
            .borrow_mut()
            .push((workspace_id.to_owned(), version));
        if self.delete_failures.get() > 0 {
            self.delete_failures.set(self.delete_failures.get() - 1);
            return Err("Aborted: version moved".into());
        }
        Ok(())
    }
}

#[test]
fn cleanup_kills_every_session_and_deletes_only_workspaces_that_still_exist() {
    let coordinator = FakeCoordinator {
        kill_refusals: vec!["s-2"],
        ..FakeCoordinator::default()
    };
    coordinator.versions.borrow_mut().insert("w-1".into(), 4);
    coordinator.delete_failures.set(1);
    let report = block_on(cleanup_created(
        &coordinator,
        vec!["s-1".into(), "s-2".into()],
        vec!["w-1".into(), "w-gone".into()],
    ));
    assert_eq!(report.killed_sessions, ["s-1"]);
    assert_eq!(report.deleted_workspaces, ["w-1"]);
    assert_eq!(report.errors, ["kill s-2: NotFound: gone"]);
    assert_eq!(
        *coordinator.deletes.borrow(),
        [("w-1".to_owned(), 4), ("w-1".to_owned(), 4)]
    );

    coordinator.delete_failures.set(2);
    let failed = block_on(cleanup_created(&coordinator, vec![], vec!["w-1".into()]));
    assert_eq!(
        failed.errors,
        ["delete workspace w-1: Aborted: version moved"]
    );
}

#[test]
fn the_created_ledger_survives_a_reload_and_ignores_what_it_cannot_read() {
    let mut ledger = CreatedResources::default();
    ledger.track_session("s-1");
    ledger.track_session("s-1");
    ledger.track_workspace("w-1");
    let restored = CreatedResources::restore(Some(&ledger.encode()));
    assert_eq!(restored, ledger);
    let numeric = CreatedResources::restore(Some(r#"{"sessions":[7],"workspaces":[]}"#));
    let mut expected = CreatedResources::default();
    expected.track_session("7");
    assert_eq!(numeric, expected);
    for unreadable in [None, Some("not json"), Some(r#"{"sessions":["s"]}"#)] {
        assert_eq!(
            CreatedResources::restore(unreadable),
            CreatedResources::default()
        );
    }
}

#[test]
fn a_workspace_takes_the_first_free_name_for_its_folder() {
    let taken = vec!["tmp".to_owned(), "tmp 2".to_owned()];
    assert_eq!(free_workspace_name(&taken, Some("tmp")), "tmp 3");
    assert_eq!(free_workspace_name(&[], Some("")), "~");
    assert_eq!(free_workspace_name(&["~".to_owned()], None), "~ 2");
}

#[test]
fn a_trusted_key_clock_starts_only_at_its_keydown_and_reports_its_duration() {
    let mut ledger = TimingLedger::default();
    assert!(
        ledger
            .begin("t-0", TimingKind::TrustedKey, None, 0.0, 0.0)
            .is_err()
    );
    ledger
        .begin(
            "t-1",
            TimingKind::TrustedKey,
            Some("s-1".into()),
            10.0,
            1000.0,
        )
        .unwrap();
    let proof = json!({ "proof_kind": "marker", "monotonicMs": 50.0 });
    let unkeyed = timing_result(&ledger.get("t-1").unwrap(), "s-1", proof.clone(), 50.0);
    assert!(
        unkeyed
            .unwrap_err()
            .contains("never observed a trusted keydown")
    );
    assert!(ledger.note_trusted_key("t-1", 20.0, 1000.0));
    assert!(!ledger.note_trusted_key("t-1", 30.0, 1000.0));
    let result = timing_result(&ledger.take("t-1").unwrap(), "s-1", proof.clone(), 50.0).unwrap();
    assert_eq!(result["durationMs"], 30.0);
    assert_eq!(
        (
            result["startedEpochMs"].clone(),
            result["trustedKey"].clone()
        ),
        (json!(1020.0), json!(true))
    );
    assert_eq!(result["proof_kind"], "marker");

    ledger
        .begin("t-2", TimingKind::Reveal, Some("s-1".into()), 5.0, 1000.0)
        .unwrap();
    let moved = timing_result(&ledger.get("t-2").unwrap(), "s-2", proof, 50.0);
    assert_eq!(
        moved.unwrap_err(),
        "terminal timing session changed: s-1 -> s-2"
    );
}

#[test]
fn the_timing_ledger_evicts_its_oldest_timing_at_capacity() {
    let mut ledger = TimingLedger::default();
    for index in 0..TIMING_CAPACITY {
        assert_eq!(
            ledger.begin(&format!("t-{index}"), TimingKind::Resize, None, 0.0, 0.0),
            Ok(None)
        );
    }
    assert_eq!(
        ledger.begin("t-new", TimingKind::Resize, None, 0.0, 0.0),
        Ok(Some("t-0".to_owned()))
    );
    assert!(ledger.get("t-0").is_none());
}

#[test]
fn a_dom_hold_needs_a_ready_generation_and_a_renderer_and_retires_with_either() {
    let generation = TerminalToken::sync(1, "sock-1", "epoch-1", 1);
    let mut holds = DomHolds::default();
    assert!(
        holds
            .arm("s", None, Some(1))
            .unwrap_err()
            .contains("ready terminal generation")
    );
    assert!(
        holds
            .arm("s", Some(generation.clone()), None)
            .unwrap_err()
            .contains("registered renderer")
    );
    let hold = holds.arm("s", Some(generation.clone()), Some(1)).unwrap();
    assert!(
        holds
            .arm("s", Some(generation.clone()), Some(1))
            .unwrap_err()
            .contains("already active")
    );
    assert!(!holds.release_if_stale("s", Some(1), Some(&generation)));
    assert!(holds.holds("s", &hold));
    assert!(holds.release_if_stale("s", Some(2), Some(&generation)));
    holds.arm("s", Some(generation.clone()), Some(2)).unwrap();
    let successor = TerminalToken::sync(2, "sock-2", "epoch-1", 1);
    assert!(holds.release_if_stale("s", Some(2), Some(&successor)));
    assert!(!holds.release("s"));
}
