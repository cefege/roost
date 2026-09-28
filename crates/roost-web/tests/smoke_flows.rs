//! The smoke backdoor's scripted flows and ledgers over fake hosts: `runFlow`'s
//! step sequence, pinning and failure attribution, `runRenderStress`'s resize
//! loop and verdicts, scoped cleanup, the created-resource ledger, the timing
//! clocks and the DOM-hold refusals. Pins `smoke::{harness, created_resources,
//! timing, dom_hold}` (v2 `smokeHarness.ts`, `smokeCreatedResources.ts`,
//! `smokeTerminalDomFault.ts`).
#![cfg(feature = "smoke")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use roost_client_core::TerminalToken;
use roost_web::smoke::call::{RenderStressOptions, TimingKind};
use roost_web::smoke::created_resources::{
    CleanupRpc, CreatedResources, cleanup_created, free_workspace_name,
};
use roost_web::smoke::dom_hold::DomHolds;
use roost_web::smoke::harness::{FlowHost, StressHost, run_flow, run_render_stress};
use roost_web::smoke::marker_scan::{SmokeMarkerScan, scan_painted_rows};
use roost_web::smoke::timing::{TIMING_CAPACITY, TimingLedger, timing_result};
use serde_json::{Value, json};

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
struct FakePage {
    workers: Vec<String>,
    spawn_error: Option<String>,
    paints: bool,
    calls: RefCell<Vec<String>>,
    frames: Cell<u32>,
}

impl FlowHost for FakePage {
    fn workers_by_recency(&self) -> Vec<String> {
        self.workers.clone()
    }
    fn has_worker(&self, worker_fp: &str) -> bool {
        self.workers.iter().any(|known| known == worker_fp)
    }
    async fn spawn_shell(&self, worker_fp: &str, folder: &str) -> Result<String, String> {
        self.calls.borrow_mut().push(format!("spawn {worker_fp} {folder}"));
        self.spawn_error.clone().map_or(Ok("s-1".to_owned()), Err)
    }
    fn open_session_route(&self, session_id: &str) {
        self.calls.borrow_mut().push(format!("route /s/{session_id}"));
    }
    fn pane_rendered(&self, _session_id: &str) -> (bool, usize) {
        (self.paints, usize::from(self.paints))
    }
    async fn create_workspace(&self, worker_fp: &str, folder: &str, session_id: &str) -> Result<Value, String> {
        self.calls.borrow_mut().push(format!("workspace {worker_fp} {folder} {session_id}"));
        Ok(json!({ "id": "w-1", "channel": 3 }))
    }
    fn marker_nonce(&self) -> String {
        "abcdef12".to_owned()
    }
    async fn input(&self, session_id: &str, text: &str) -> Result<(), String> {
        self.calls.borrow_mut().push(format!("input {session_id} {text:?}"));
        Ok(())
    }
    async fn wait_for_painted_marker(&self, _session_id: &str, marker: &str) -> Result<Value, String> {
        if self.paints { Ok(json!({ "marker": marker })) } else { Err("marker was not visibly painted".to_owned()) }
    }
    async fn terminal_stream_probe(&self, _session_id: &str) -> Result<Value, String> {
        Err("U-2 TERMINAL DIAG".to_owned())
    }
    async fn cleanup_created(&self) -> (Value, bool) {
        self.calls.borrow_mut().push("cleanup".to_owned());
        (json!({ "errors": [] }), true)
    }
    async fn next_frame(&self) {
        self.frames.set(self.frames.get() + 1);
    }
}

fn names(steps: &[roost_web::smoke::harness::Step]) -> Vec<&'static str> {
    steps.iter().map(|step| step.name).collect()
}

#[test]
fn the_flow_spawns_routes_paints_round_trips_a_marker_and_cleans_up() {
    let page = FakePage { workers: vec!["fp-new".into(), "fp-old".into()], paints: true, ..FakePage::default() };
    let report = block_on(run_flow(&page, None));
    assert_eq!(
        names(&report.steps),
        ["worker_available", "shell_painted", "workspace_created", "shell_round_trip", "cleanup"]
    );
    assert_eq!(report.summary, "5/5 passed");
    assert_eq!(
        *page.calls.borrow(),
        [
            "spawn fp-new /tmp",
            "route /s/s-1",
            "workspace fp-new / s-1",
            "input s-1 \"printf '%s\\\\n' ROOST_SMOKE_abcdef12\\n\"",
            "cleanup",
        ]
    );
    assert_eq!(report.steps[3].detail, json!({ "marker": "ROOST_SMOKE_abcdef12" }));
}

#[test]
fn a_pinned_worker_wins_over_the_most_recent_one() {
    let page = FakePage { workers: vec!["fp-new".into(), "fp-pty".into()], paints: true, ..FakePage::default() };
    block_on(run_flow(&page, Some("fp-pty".into())));
    assert_eq!(page.calls.borrow()[0], "spawn fp-pty /tmp");
}

#[test]
fn with_no_worker_the_flow_reports_zero_of_one_and_still_cleans_up() {
    let page = FakePage::default();
    let report = block_on(run_flow(&page, None));
    assert_eq!(names(&report.steps), ["worker_available", "cleanup"]);
    assert!(!report.steps[0].pass);
    assert_eq!(report.summary, "0/1 passed");
}

#[test]
fn a_failure_is_recorded_with_its_layers_and_cleanup_still_runs() {
    let refused = FakePage { workers: vec!["fp".into()], spawn_error: Some("PermissionDenied: no".into()), ..FakePage::default() };
    let report = block_on(run_flow(&refused, None));
    assert_eq!(names(&report.steps), ["worker_available", "flow_exception", "cleanup"]);
    assert_eq!(report.steps[1].detail, json!({ "error": "Error: PermissionDenied: no", "layers": null }));
    assert_eq!(report.summary, "2/3 passed");

    let silent = FakePage { workers: vec!["fp".into()], ..FakePage::default() };
    let report = block_on(run_flow(&silent, None));
    assert!(!report.steps[1].pass, "a pane that never mounts fails shell_painted");
    assert_eq!(silent.frames.get(), 600);
    let exception = report.steps.iter().find(|step| step.name == "flow_exception").unwrap();
    assert_eq!(exception.detail["layers"], json!({ "probe_failed": "Error: U-2 TERMINAL DIAG" }));
}

struct FakeDeck {
    present: bool,
    size: Cell<(f64, f64)>,
    sizes: RefCell<Vec<(i64, i64)>>,
    restored: RefCell<Option<Option<String>>>,
    scans: RefCell<Vec<Vec<&'static str>>>,
    frames: Cell<u64>,
}

impl StressHost for FakeDeck {
    fn deck_size(&self) -> Option<(f64, f64)> {
        self.present.then(|| self.size.get())
    }
    fn deck_style(&self) -> Option<String> {
        Some("flex: 1".to_owned())
    }
    fn set_deck_size(&self, width_px: i64, height_px: i64) {
        self.sizes.borrow_mut().push((width_px, height_px));
        self.size.set((width_px as f64, height_px as f64));
    }
    fn restore_deck_style(&self, original: Option<&str>) {
        *self.restored.borrow_mut() = Some(original.map(str::to_owned));
    }
    fn marker_scan(&self, _session_id: &str, prefix: &str) -> SmokeMarkerScan {
        let mut scans = self.scans.borrow_mut();
        let rows = if scans.len() > 1 { scans.remove(0) } else { scans[0].clone() };
        scan_painted_rows(rows, prefix)
    }
    fn cell_frame_count(&self, _session_id: &str) -> u64 {
        self.frames.get()
    }
    fn renders_cells(&self, _session_id: &str) -> bool {
        true
    }
    async fn next_frame(&self) {
        self.frames.set(self.frames.get() + 1);
    }
}

fn deck(present: bool, scans: Vec<Vec<&'static str>>) -> FakeDeck {
    FakeDeck {
        present,
        size: Cell::new((300.0, 250.0)),
        sizes: RefCell::new(Vec::new()),
        restored: RefCell::new(None),
        scans: RefCell::new(scans),
        frames: Cell::new(0),
    }
}

fn stress(main_screen: bool, iterations: u32) -> RenderStressOptions {
    RenderStressOptions { session_id: "s-1".into(), prefix: "M".into(), main_screen, iterations }
}

#[test]
fn render_stress_without_a_deck_fails_without_iterating() {
    let report = block_on(run_render_stress(&deck(false, vec![vec![]]), &stress(true, 3)));
    assert_eq!((report.verdict, report.iterations, report.fail_count), ("FAIL", 0, 1));
    assert_eq!(report.fails, vec![json!("terminal deck missing")]);
}

#[test]
fn render_stress_perturbs_the_deck_within_its_floor_and_restores_its_style() {
    let page = deck(true, vec![vec!["M1", "M2"]]);
    let report = block_on(run_render_stress(&page, &stress(true, 4)));
    assert_eq!(report.verdict, "PASS");
    assert_eq!(*page.sizes.borrow(), [(220, 250), (420, 250), (420, 180), (420, 360)]);
    assert_eq!(*page.restored.borrow(), Some(Some("flex: 1".to_owned())));
}

#[test]
fn a_moved_newest_marker_fails_the_main_screen_but_not_the_alternate_one() {
    let lost = vec![vec!["M1", "M2", "M3"], vec!["M1", "M2"]];
    let main = block_on(run_render_stress(&deck(true, lost.clone()), &stress(true, 1)));
    assert_eq!((main.verdict, main.fail_count), ("FAIL", 1));
    assert_eq!(main.fails[0]["screen"], "main");
    let alt = block_on(run_render_stress(&deck(true, lost), &stress(false, 1)));
    assert_eq!(alt.verdict, "PASS");
    let duplicated = block_on(run_render_stress(&deck(true, vec![vec!["M1"], vec!["M1", "M1"]]), &stress(false, 1)));
    assert_eq!(duplicated.verdict, "FAIL");
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
        if self.kill_refusals.contains(&session_id) { Err("NotFound: gone".into()) } else { Ok(()) }
    }
    async fn workspace_versions(&self) -> Result<BTreeMap<String, u64>, String> {
        Ok(self.versions.borrow().clone())
    }
    async fn delete_workspace(&self, workspace_id: &str, version: u64) -> Result<(), String> {
        self.deletes.borrow_mut().push((workspace_id.to_owned(), version));
        if self.delete_failures.get() > 0 {
            self.delete_failures.set(self.delete_failures.get() - 1);
            return Err("Aborted: version moved".into());
        }
        Ok(())
    }
}

#[test]
fn cleanup_kills_every_session_and_deletes_only_workspaces_that_still_exist() {
    let coordinator = FakeCoordinator { kill_refusals: vec!["s-2"], ..FakeCoordinator::default() };
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
    assert_eq!(*coordinator.deletes.borrow(), [("w-1".to_owned(), 4), ("w-1".to_owned(), 4)]);

    coordinator.delete_failures.set(2);
    let failed = block_on(cleanup_created(&coordinator, vec![], vec!["w-1".into()]));
    assert_eq!(failed.errors, ["delete workspace w-1: Aborted: version moved"]);
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
        assert_eq!(CreatedResources::restore(unreadable), CreatedResources::default());
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
    assert!(ledger.begin("t-0", TimingKind::TrustedKey, None, 0.0, 0.0).is_err());
    ledger.begin("t-1", TimingKind::TrustedKey, Some("s-1".into()), 10.0, 1000.0).unwrap();
    let proof = json!({ "proof_kind": "marker", "monotonicMs": 50.0 });
    let unkeyed = timing_result(&ledger.get("t-1").unwrap(), "s-1", proof.clone(), 50.0);
    assert!(unkeyed.unwrap_err().contains("never observed a trusted keydown"));
    assert!(ledger.note_trusted_key("t-1", 20.0, 1000.0));
    assert!(!ledger.note_trusted_key("t-1", 30.0, 1000.0));
    let result = timing_result(&ledger.take("t-1").unwrap(), "s-1", proof.clone(), 50.0).unwrap();
    assert_eq!(result["durationMs"], 30.0);
    assert_eq!((result["startedEpochMs"].clone(), result["trustedKey"].clone()), (json!(1020.0), json!(true)));
    assert_eq!(result["proof_kind"], "marker");

    ledger.begin("t-2", TimingKind::Reveal, Some("s-1".into()), 5.0, 1000.0).unwrap();
    let moved = timing_result(&ledger.get("t-2").unwrap(), "s-2", proof, 50.0);
    assert_eq!(moved.unwrap_err(), "terminal timing session changed: s-1 -> s-2");
}

#[test]
fn the_timing_ledger_evicts_its_oldest_timing_at_capacity() {
    let mut ledger = TimingLedger::default();
    for index in 0..TIMING_CAPACITY {
        assert_eq!(ledger.begin(&format!("t-{index}"), TimingKind::Resize, None, 0.0, 0.0), Ok(None));
    }
    assert_eq!(ledger.begin("t-new", TimingKind::Resize, None, 0.0, 0.0), Ok(Some("t-0".to_owned())));
    assert!(ledger.get("t-0").is_none());
}

#[test]
fn a_dom_hold_needs_a_ready_generation_and_a_renderer_and_retires_with_either() {
    let generation = TerminalToken::sync(1, "sock-1", "epoch-1", 1);
    let mut holds = DomHolds::default();
    assert!(holds.arm("s", None, Some(1)).unwrap_err().contains("ready terminal generation"));
    assert!(holds.arm("s", Some(generation.clone()), None).unwrap_err().contains("registered renderer"));
    let hold = holds.arm("s", Some(generation.clone()), Some(1)).unwrap();
    assert!(holds.arm("s", Some(generation.clone()), Some(1)).unwrap_err().contains("already active"));
    assert!(!holds.release_if_stale("s", Some(1), Some(&generation)));
    assert!(holds.holds("s", &hold));
    assert!(holds.release_if_stale("s", Some(2), Some(&generation)));
    holds.arm("s", Some(generation.clone()), Some(2)).unwrap();
    let successor = TerminalToken::sync(2, "sock-2", "epoch-1", 1);
    assert!(holds.release_if_stale("s", Some(2), Some(&successor)));
    assert!(!holds.release("s"));
}
