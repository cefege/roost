//! The smoke backdoor's scripted flows over fake hosts: `runFlow`'s step
//! sequence, pinning and failure attribution, and `runRenderStress`'s resize
//! loop and verdicts. Pins `smoke::harness` (v2 `smokeHarness.ts`); the
//! ledgers are in `smoke_ledgers.rs`.
#![cfg(feature = "smoke")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::{Cell, RefCell};
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use roost_web::smoke::call::RenderStressOptions;
use roost_web::smoke::harness::{FlowHost, StressHost, run_flow, run_render_stress};
use roost_web::smoke::marker_scan::{SmokeMarkerScan, scan_painted_rows};
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
        self.calls
            .borrow_mut()
            .push(format!("spawn {worker_fp} {folder}"));
        self.spawn_error.clone().map_or(Ok("s-1".to_owned()), Err)
    }
    fn open_session_route(&self, session_id: &str) {
        self.calls
            .borrow_mut()
            .push(format!("route /s/{session_id}"));
    }
    fn pane_rendered(&self, _session_id: &str) -> (bool, usize) {
        (self.paints, usize::from(self.paints))
    }
    async fn create_workspace(
        &self,
        worker_fp: &str,
        folder: &str,
        session_id: &str,
    ) -> Result<Value, String> {
        self.calls
            .borrow_mut()
            .push(format!("workspace {worker_fp} {folder} {session_id}"));
        Ok(json!({ "id": "w-1", "channel": 3 }))
    }
    fn marker_nonce(&self) -> String {
        "abcdef12".to_owned()
    }
    async fn input(&self, session_id: &str, text: &str) -> Result<(), String> {
        self.calls
            .borrow_mut()
            .push(format!("input {session_id} {text:?}"));
        Ok(())
    }
    async fn wait_for_painted_marker(
        &self,
        _session_id: &str,
        marker: &str,
    ) -> Result<Value, String> {
        if self.paints {
            Ok(json!({ "marker": marker }))
        } else {
            Err("marker was not visibly painted".to_owned())
        }
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
    let page = FakePage {
        workers: vec!["fp-new".into(), "fp-old".into()],
        paints: true,
        ..FakePage::default()
    };
    let report = block_on(run_flow(&page, None));
    assert_eq!(
        names(&report.steps),
        [
            "worker_available",
            "shell_painted",
            "workspace_created",
            "shell_round_trip",
            "cleanup"
        ]
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
    assert_eq!(
        report.steps[3].detail,
        json!({ "marker": "ROOST_SMOKE_abcdef12" })
    );
}

#[test]
fn a_pinned_worker_wins_over_the_most_recent_one() {
    let page = FakePage {
        workers: vec!["fp-new".into(), "fp-pty".into()],
        paints: true,
        ..FakePage::default()
    };
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
    let refused = FakePage {
        workers: vec!["fp".into()],
        spawn_error: Some("PermissionDenied: no".into()),
        ..FakePage::default()
    };
    let report = block_on(run_flow(&refused, None));
    assert_eq!(
        names(&report.steps),
        ["worker_available", "flow_exception", "cleanup"]
    );
    assert_eq!(
        report.steps[1].detail,
        json!({ "error": "Error: PermissionDenied: no", "layers": null })
    );
    assert_eq!(report.summary, "2/3 passed");

    let silent = FakePage {
        workers: vec!["fp".into()],
        ..FakePage::default()
    };
    let report = block_on(run_flow(&silent, None));
    assert!(
        !report.steps[1].pass,
        "a pane that never mounts fails shell_painted"
    );
    assert_eq!(silent.frames.get(), 600);
    let exception = report
        .steps
        .iter()
        .find(|step| step.name == "flow_exception")
        .unwrap();
    assert_eq!(
        exception.detail["layers"],
        json!({ "probe_failed": "Error: U-2 TERMINAL DIAG" })
    );
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
        let rows = if scans.len() > 1 {
            scans.remove(0)
        } else {
            scans[0].clone()
        };
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
    RenderStressOptions {
        session_id: "s-1".into(),
        prefix: "M".into(),
        main_screen,
        iterations,
    }
}

#[test]
fn render_stress_without_a_deck_fails_without_iterating() {
    let report = block_on(run_render_stress(
        &deck(false, vec![vec![]]),
        &stress(true, 3),
    ));
    assert_eq!(
        (report.verdict, report.iterations, report.fail_count),
        ("FAIL", 0, 1)
    );
    assert_eq!(report.fails, vec![json!("terminal deck missing")]);
}

#[test]
fn render_stress_perturbs_the_deck_within_its_floor_and_restores_its_style() {
    let page = deck(true, vec![vec!["M1", "M2"]]);
    let report = block_on(run_render_stress(&page, &stress(true, 4)));
    assert_eq!(report.verdict, "PASS");
    assert_eq!(
        *page.sizes.borrow(),
        [(220, 250), (420, 250), (420, 180), (420, 360)]
    );
    assert_eq!(*page.restored.borrow(), Some(Some("flex: 1".to_owned())));
}

#[test]
fn a_moved_newest_marker_fails_the_main_screen_but_not_the_alternate_one() {
    let lost = vec![vec!["M1", "M2", "M3"], vec!["M1", "M2"]];
    let main = block_on(run_render_stress(
        &deck(true, lost.clone()),
        &stress(true, 1),
    ));
    assert_eq!((main.verdict, main.fail_count), ("FAIL", 1));
    assert_eq!(main.fails[0]["screen"], "main");
    let alt = block_on(run_render_stress(&deck(true, lost), &stress(false, 1)));
    assert_eq!(alt.verdict, "PASS");
    let duplicated = block_on(run_render_stress(
        &deck(true, vec![vec!["M1"], vec!["M1", "M1"]]),
        &stress(false, 1),
    ));
    assert_eq!(duplicated.verdict, "FAIL");
}
