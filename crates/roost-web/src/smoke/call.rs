//! The `window.__smoke` method table and the argument grammar of every member:
//! which names exist, which answer with a Promise, and the typed call each JS
//! argument list parses into. Native; `smoke::backdoor` binds one JS function per
//! name and hands the parsed call to `smoke::dispatch`. The member list is
//! `SmokeApi` in `apps/web/src/smoke/smokeTypes.ts:118-287`, assembled by
//! `apps/web/src/smoke/smoke.ts:50-117`.

use serde_json::Value;

use super::call_args::{
    Args, cursor_coordinate, optional_field_string, render_stress_options, timing_kind,
};

/// How a member answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// A plain value, or a thrown `Error`.
    Sync,
    /// A Promise that resolves or rejects.
    Promise,
}

/// Every `SmokeApi` member, in `smokeTypes.ts` order.
pub const SMOKE_METHODS: [(&str, Answer); 53] = [
    ("input", Answer::Promise),
    ("terminalInputCapture", Answer::Sync),
    ("resetTerminalInputCapture", Answer::Sync),
    ("paneFocused", Answer::Sync),
    ("viewportText", Answer::Sync),
    ("renderProbe", Answer::Sync),
    ("paintedScrollback", Answer::Sync),
    ("hasPaintedScrollbackRange", Answer::Sync),
    ("paintedScrollbackRange", Answer::Sync),
    ("markerScan", Answer::Sync),
    ("waitForPaintedMarker", Answer::Promise),
    ("waitForPaintedCursor", Answer::Promise),
    ("terminalBrowserSnapshot", Answer::Sync),
    ("terminalStreamProbe", Answer::Promise),
    ("probeTerminalTransport", Answer::Promise),
    ("beginTerminalTiming", Answer::Promise),
    ("finishTerminalTiming", Answer::Promise),
    ("terminalDimensions", Answer::Sync),
    ("phaseTimeline", Answer::Sync),
    ("retainedMarkerScan", Answer::Promise),
    ("state", Answer::Sync),
    ("forceVisible", Answer::Sync),
    ("forceHidden", Answer::Sync),
    ("forceSyncMaxBackoff", Answer::Sync),
    ("syncRedialStatus", Answer::Sync),
    ("pauseSyncTransport", Answer::Sync),
    ("resumeSyncTransport", Answer::Sync),
    ("cellFrameCount", Answer::Sync),
    ("cellFullFrameCount", Answer::Sync),
    ("lastFullFrameSbRows", Answer::Sync),
    ("scrollbackBackfillRequestCount", Answer::Sync),
    ("directHistoryResponseCount", Answer::Sync),
    ("cellGridEpoch", Answer::Sync),
    ("blackholeTerminalFramesForCurrentGeneration", Answer::Sync),
    ("dropNextTerminalWireDelta", Answer::Sync),
    ("dropNextCellFrame", Answer::Sync),
    ("droppedCellFrameCount", Answer::Sync),
    ("holdTerminalDomForCurrentGeneration", Answer::Sync),
    ("releaseTerminalDomHold", Answer::Sync),
    ("syncWsGeneration", Answer::Sync),
    ("navigate", Answer::Sync),
    ("kill", Answer::Promise),
    ("spawnShell", Answer::Promise),
    ("createWorkspace", Answer::Promise),
    ("trackCreatedSession", Answer::Sync),
    ("cleanupCreated", Answer::Promise),
    ("runFlow", Answer::Promise),
    ("runRenderStress", Answer::Promise),
    ("uploadAttachment", Answer::Promise),
    ("attachmentProbe", Answer::Promise),
    ("downloadWorkerFile", Answer::Promise),
    ("perfProbe", Answer::Sync),
    ("resetPerfCounters", Answer::Sync),
];

/// The members whose surface belongs to a slice this build does not have yet,
/// and the refusal each one answers with — never a silent no-op.
pub const UNPORTED_METHODS: [(&str, &str); 10] = [
    ("terminalBrowserSnapshot", "U-2 TERMINAL DIAG: terminalBrowserStreamSnapshot (renderer/terminalDiagSnapshot.ts) not ported"),
    ("terminalStreamProbe", "U-2 TERMINAL DIAG: the browser layer of the stream probe (renderer/terminalDiagSnapshot.ts) not ported"),
    ("probeTerminalTransport", "U-2 STREAM LIFECYCLE: the worker control probe (store/transport/sync-terminal-control-probe.ts) not ported"),
    ("phaseTimeline", "U-2 BROWSER platform: phase marks (browser/diag.ts phaseTimeline) not ported"),
    ("forceVisible", "U-2 BROWSER platform: the visibility pin (browser/pageVisible.ts) not ported"),
    ("forceHidden", "U-2 BROWSER platform: the visibility pin (browser/pageVisible.ts) not ported"),
    ("directHistoryResponseCount", "U-2 CARRIER/LOCAL: direct-carrier history reads (lib/scrollbackDirectHistory.ts) not ported"),
    ("uploadAttachment", "U-2 ATTACH: the chunked uploadAttachment path (lib/attachments.ts) not ported"),
    ("perfProbe", "U-2 BROWSER platform: the leak watcher (browser/leakWatch.ts) not ported"),
    ("resetPerfCounters", "U-2 BROWSER platform: the leak watcher (browser/leakWatch.ts) not ported"),
];

/// The refusal of a member whose surface is not in this build.
pub fn unported_refusal(name: &str) -> Option<&'static str> {
    UNPORTED_METHODS
        .iter()
        .find(|(method, _)| *method == name)
        .map(|(_, refusal)| *refusal)
}

/// Which timing clock `beginTerminalTiming` starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimingKind {
    TrustedKey,
    Reveal,
    Resize,
    Optimistic,
}

impl TimingKind {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TrustedKey => "trusted_key",
            Self::Reveal => "reveal",
            Self::Resize => "resize",
            Self::Optimistic => "optimistic",
        }
    }
}

/// `runRenderStress` options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderStressOptions {
    pub session_id: String,
    pub prefix: String,
    /// `true` for `"main"`: only the main screen must keep its newest marker.
    pub main_screen: bool,
    pub iterations: u32,
}

/// One parsed member call.
#[derive(Debug, Clone, PartialEq)]
pub enum SmokeCall {
    /// A member whose surface is not in this build, with its refusal.
    Unported { refusal: &'static str },
    /// A member taking no arguments.
    Bare { method: &'static str },
    /// A member taking only a session id.
    Session { method: &'static str, session_id: String },
    Input { session_id: String, text: String },
    ScrollbackRange { method: &'static str, session_id: String, start: u32, end: u32 },
    MarkerScan { session_id: String, prefix: String },
    WaitForPaintedMarker { session_id: String, marker: String, timeout_ms: f64 },
    WaitForPaintedCursor { session_id: String, row: Option<u32>, column: Option<u32>, timeout_ms: f64 },
    BeginTiming { kind: TimingKind, session_id: Option<String> },
    FinishTiming { timing_id: String, session_id: String, marker: String, timeout_ms: f64 },
    RetainedMarkerScan { session_id: String, prefix: String, page_rows: Option<f64> },
    Navigate { href: String },
    SpawnShell { worker_fp: String, folder: String, session_id: Option<String> },
    CreateWorkspace { worker_fp: String, folder: String, session_id: String },
    RunFlow { worker_fp: Option<String> },
    RunRenderStress(RenderStressOptions),
    AttachmentProbe { session_id: String, sha256: String, size: u64, filename: String },
    DownloadWorkerFile { worker_fp: String, path: String },
}

/// The default paint-proof deadline, as v2's `timeoutMs = 30_000`.
pub const DEFAULT_PAINT_TIMEOUT_MS: f64 = 30_000.0;

/// Parse `name(args…)`. `args` are the JS arguments as JSON, `Null` for an
/// omitted one; the error is the `TypeError`-style message the call answers.
pub fn parse_call(name: &str, args: &[Value]) -> Result<SmokeCall, String> {
    if let Some(refusal) = unported_refusal(name) {
        return Ok(SmokeCall::Unported { refusal });
    }
    let Some(&(method, _)) = SMOKE_METHODS.iter().find(|(known, _)| *known == name) else {
        return Err(format!("__smoke has no member {name}"));
    };
    let arg = Args { method, args };
    Ok(match method {
        "terminalInputCapture" | "resetTerminalInputCapture" | "state" | "forceSyncMaxBackoff"
        | "syncRedialStatus" | "pauseSyncTransport" | "resumeSyncTransport"
        | "syncWsGeneration" | "cleanupCreated" => SmokeCall::Bare { method },
        "input" => SmokeCall::Input { session_id: arg.string(0)?, text: arg.string(1)? },
        "hasPaintedScrollbackRange" | "paintedScrollbackRange" => SmokeCall::ScrollbackRange {
            method,
            session_id: arg.string(0)?,
            start: arg.row(1)?,
            end: arg.row(2)?,
        },
        "markerScan" => SmokeCall::MarkerScan { session_id: arg.string(0)?, prefix: arg.string(1)? },
        "waitForPaintedMarker" => SmokeCall::WaitForPaintedMarker {
            session_id: arg.string(0)?,
            marker: arg.string(1)?,
            timeout_ms: arg.number_or(2, DEFAULT_PAINT_TIMEOUT_MS)?,
        },
        "waitForPaintedCursor" => {
            let expected = arg.object_or_null(1)?;
            SmokeCall::WaitForPaintedCursor {
                session_id: arg.string(0)?,
                row: cursor_coordinate(expected, "row")?,
                column: cursor_coordinate(expected, "column")?,
                timeout_ms: arg.number_or(2, DEFAULT_PAINT_TIMEOUT_MS)?,
            }
        }
        "beginTerminalTiming" => SmokeCall::BeginTiming {
            kind: timing_kind(&arg.string(0)?)?,
            session_id: arg.optional_string(1)?,
        },
        "finishTerminalTiming" => SmokeCall::FinishTiming {
            timing_id: arg.string(0)?,
            session_id: arg.string(1)?,
            marker: arg.string(2)?,
            timeout_ms: arg.number_or(3, DEFAULT_PAINT_TIMEOUT_MS)?,
        },
        "retainedMarkerScan" => SmokeCall::RetainedMarkerScan {
            session_id: arg.string(0)?,
            prefix: arg.string(1)?,
            page_rows: arg.optional_number(2)?,
        },
        "navigate" => SmokeCall::Navigate { href: arg.string(0)? },
        "spawnShell" => SmokeCall::SpawnShell {
            worker_fp: arg.string(0)?,
            folder: arg.string(1)?,
            session_id: arg.optional_string(2)?,
        },
        "createWorkspace" => SmokeCall::CreateWorkspace {
            worker_fp: arg.string(0)?,
            folder: arg.string(1)?,
            session_id: arg.string(2)?,
        },
        "runFlow" => SmokeCall::RunFlow {
            worker_fp: match arg.object_or_null(0)? {
                Some(options) => optional_field_string(method, options, "workerFp")?,
                None => None,
            },
        },
        "runRenderStress" => SmokeCall::RunRenderStress(render_stress_options(&arg)?),
        "attachmentProbe" => SmokeCall::AttachmentProbe {
            session_id: arg.string(0)?,
            sha256: arg.string(1)?,
            size: arg.count(2)?,
            filename: arg.optional_string(3)?.unwrap_or_else(|| "probe.bin".to_owned()),
        },
        "downloadWorkerFile" => SmokeCall::DownloadWorkerFile {
            worker_fp: arg.string(0)?,
            path: arg.string(1)?,
        },
        _ => SmokeCall::Session { method, session_id: arg.string(0)? },
    })
}
