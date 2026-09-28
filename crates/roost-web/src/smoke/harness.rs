//! The two scripted smoke scenarios, as native state machines over a host:
//! `runFlow` (spawn → route → paint → workspace → PTY marker round trip →
//! cleanup) and `runRenderStress` (deck resize loop with marker continuity).
//! `smoke::dispatch` runs them with the backdoor itself as the host, exactly as
//! v2 ran them against `api`. Ports `apps/web/src/smoke/smokeHarness.ts:23-29,524-612`.

use serde::Serialize;
use serde_json::{Value, json};

use super::call::RenderStressOptions;
use super::marker_scan::SmokeMarkerScan;

/// One recorded step.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Step {
    pub name: &'static str,
    pub pass: bool,
    pub detail: Value,
}

/// What `runFlow` answers.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FlowReport {
    pub steps: Vec<Step>,
    pub summary: String,
}

/// What `runRenderStress` answers.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StressReport {
    pub verdict: &'static str,
    pub iterations: u32,
    pub fail_count: usize,
    pub fails: Vec<Value>,
}

/// Frames `runFlow` waits for a pane to mount, and then to paint a row.
pub const FLOW_PAINT_FRAMES: u32 = 300;
/// Frames a stress iteration waits for the resize's cell frame.
pub const STRESS_FRAME_WAIT: u32 = 120;
/// The six deck perturbations the stress loop cycles through.
pub const STRESS_PERTURBATIONS: [(f64, f64); 6] = [
    (-200.0, 0.0),
    (200.0, 0.0),
    (0.0, -180.0),
    (0.0, 180.0),
    (-200.0, -180.0),
    (200.0, 180.0),
];

/// Everything `runFlow` asks of the page: the `SmokeApi` members it calls.
pub trait FlowHost {
    /// Worker fingerprints, most recently seen first.
    fn workers_by_recency(&self) -> Vec<String>;
    fn has_worker(&self, worker_fp: &str) -> bool;
    /// `spawnShell(fp, folder)`: the session id, or the call's error.
    fn spawn_shell(
        &self,
        worker_fp: &str,
        folder: &str,
    ) -> impl Future<Output = Result<String, String>>;
    /// Route the page to `/s/<session>` the way a reader's navigation does.
    fn open_session_route(&self, session_id: &str);
    /// `renderProbe(session).found` and `.nonEmptyRows`.
    fn pane_rendered(&self, session_id: &str) -> (bool, usize);
    /// `createWorkspace(fp, folder, session)` as its JSON answer.
    fn create_workspace(
        &self,
        worker_fp: &str,
        folder: &str,
        session_id: &str,
    ) -> impl Future<Output = Result<Value, String>>;
    /// Eight fresh hex characters for the marker.
    fn marker_nonce(&self) -> String;
    fn input(&self, session_id: &str, text: &str) -> impl Future<Output = Result<(), String>>;
    /// `waitForPaintedMarker(session, marker)` as its proof JSON.
    fn wait_for_painted_marker(
        &self,
        session_id: &str,
        marker: &str,
    ) -> impl Future<Output = Result<Value, String>>;
    /// `terminalStreamProbe(session)` as its JSON answer.
    fn terminal_stream_probe(
        &self,
        session_id: &str,
    ) -> impl Future<Output = Result<Value, String>>;
    /// `cleanupCreated()`: its JSON answer and whether it recorded no error.
    fn cleanup_created(&self) -> impl Future<Output = (Value, bool)>;
    fn next_frame(&self) -> impl Future<Output = ()>;
}

/// `waitFor(check, frameLimit)`: true as soon as `check` holds, checked once
/// more after the last frame.
pub async fn wait_frames_for<H: FlowHost>(
    host: &H,
    frame_limit: u32,
    check: impl Fn(&H) -> bool,
) -> bool {
    for _ in 0..frame_limit {
        if check(host) {
            return true;
        }
        host.next_frame().await;
    }
    check(host)
}

/// `runFlow(api, { workerFp })`.
pub async fn run_flow<H: FlowHost>(host: &H, pinned_worker: Option<String>) -> FlowReport {
    let mut steps = Vec::new();
    let mut session_id = None;
    let mut early_summary = None;
    if let Err(error) = flow_body(
        host,
        pinned_worker,
        &mut steps,
        &mut session_id,
        &mut early_summary,
    )
    .await
    {
        let layers = match &session_id {
            Some(id) => match host.terminal_stream_probe(id).await {
                Ok(layers) => layers,
                Err(probe_error) => json!({ "probe_failed": js_error_string(&probe_error) }),
            },
            None => Value::Null,
        };
        steps.push(Step {
            name: "flow_exception",
            pass: false,
            detail: json!({ "error": js_error_string(&error), "layers": layers }),
        });
    }
    let (cleanup, clean) = host.cleanup_created().await;
    steps.push(Step {
        name: "cleanup",
        pass: clean,
        detail: cleanup,
    });
    let summary = early_summary.unwrap_or_else(|| {
        let passed = steps.iter().filter(|step| step.pass).count();
        format!("{passed}/{} passed", steps.len())
    });
    tracing::info!(target: "smoke", %summary, "runFlow finished");
    FlowReport { steps, summary }
}

async fn flow_body<H: FlowHost>(
    host: &H,
    pinned_worker: Option<String>,
    steps: &mut Vec<Step>,
    session_slot: &mut Option<String>,
    early_summary: &mut Option<String>,
) -> Result<(), String> {
    let worker_fp = pinned_worker.or_else(|| host.workers_by_recency().into_iter().next());
    let available = worker_fp.as_deref().is_some_and(|fp| host.has_worker(fp));
    steps.push(Step {
        name: "worker_available",
        pass: available,
        detail: worker_fp
            .as_ref()
            .map_or_else(|| json!({}), |fp| json!({ "workerFp": fp })),
    });
    // v2 returns "0/1 passed" here while its `finally` still appends cleanup.
    let Some(worker_fp) = worker_fp else {
        *early_summary = Some("0/1 passed".to_owned());
        return Ok(());
    };
    let session_id = host.spawn_shell(&worker_fp, "/tmp").await?;
    *session_slot = Some(session_id.clone());
    host.open_session_route(&session_id);
    let mounted = wait_frames_for(host, FLOW_PAINT_FRAMES, |host| {
        host.pane_rendered(&session_id).0
    })
    .await;
    steps.push(Step {
        name: "shell_painted",
        pass: mounted,
        detail: json!({ "sessionId": session_id }),
    });
    wait_frames_for(host, FLOW_PAINT_FRAMES, |host| {
        host.pane_rendered(&session_id).1 > 0
    })
    .await;

    let workspace = host.create_workspace(&worker_fp, "/", &session_id).await?;
    let has_id = workspace
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.is_empty());
    steps.push(Step {
        name: "workspace_created",
        pass: has_id,
        detail: workspace,
    });

    let marker = format!("ROOST_SMOKE_{}", host.marker_nonce());
    host.input(&session_id, &format!("printf '%s\\n' {marker}\n"))
        .await?;
    let painted = host.wait_for_painted_marker(&session_id, &marker).await?;
    steps.push(Step {
        name: "shell_round_trip",
        pass: true,
        detail: painted,
    });
    Ok(())
}

/// `String(error)` of a thrown `Error`.
pub fn js_error_string(message: &str) -> String {
    format!("Error: {message}")
}

/// Everything `runRenderStress` asks of the page.
pub trait StressHost {
    /// The terminal deck's rendered size, or `None` when no deck is mounted.
    fn deck_size(&self) -> Option<(f64, f64)>;
    /// The deck's `style` attribute before the loop touched it.
    fn deck_style(&self) -> Option<String>;
    fn set_deck_size(&self, width_px: i64, height_px: i64);
    fn restore_deck_style(&self, original: Option<&str>);
    fn marker_scan(&self, session_id: &str, prefix: &str) -> SmokeMarkerScan;
    fn cell_frame_count(&self, session_id: &str) -> u64;
    /// `renderProbe(session).mode === "cell"`.
    fn renders_cells(&self, session_id: &str) -> bool;
    fn next_frame(&self) -> impl Future<Output = ()>;
}

/// `runRenderStress(api, options)`.
pub async fn run_render_stress<H: StressHost>(
    host: &H,
    options: &RenderStressOptions,
) -> StressReport {
    if host.deck_size().is_none() {
        return StressReport {
            verdict: "FAIL",
            iterations: 0,
            fail_count: 1,
            fails: vec![json!("terminal deck missing")],
        };
    }
    let original = host.deck_style();
    let screen = if options.main_screen { "main" } else { "alt" };
    let sid = options.session_id.as_str();
    let baseline = host.marker_scan(sid, &options.prefix);
    let mut fails = Vec::new();
    for iteration in 0..options.iterations {
        let (dx, dy) = STRESS_PERTURBATIONS[iteration as usize % STRESS_PERTURBATIONS.len()];
        if let Some((width, height)) = host.deck_size() {
            let width = super::probes::js_round(width + dx).max(220);
            let height = super::probes::js_round(height + dy).max(180);
            host.set_deck_size(width, height);
        }
        let before = host.cell_frame_count(sid);
        if host.renders_cells(sid) {
            for _ in 0..STRESS_FRAME_WAIT {
                if host.cell_frame_count(sid) > before {
                    break;
                }
                host.next_frame().await;
            }
        } else {
            frames(host, 6).await;
        }
        frames(host, 2).await;
        let scan = host.marker_scan(sid, &options.prefix);
        // A shorter deck shows fewer rows, so a rising min is normal; a moving
        // max means live history was lost rather than scrolled out.
        if !scan.duplicated.is_empty()
            || scan.out_of_order > 0
            || (options.main_screen && scan.max != baseline.max)
        {
            fails.push(json!({ "iteration": iteration, "screen": screen, "scan": scan, "baseline": baseline }));
        }
    }
    host.restore_deck_style(original.as_deref());
    StressReport {
        verdict: if fails.is_empty() { "PASS" } else { "FAIL" },
        iterations: options.iterations,
        fail_count: fails.len(),
        fails,
    }
}

async fn frames<H: StressHost>(host: &H, count: u32) {
    for _ in 0..count {
        host.next_frame().await;
    }
}
