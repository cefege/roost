//! Answering one parsed `window.__smoke` call: the synchronous reads of the
//! store, the pane registry and the DOM, the transport and renderer fault arms,
//! and the hand-off of every Promise member to `smoke::rpc_calls`,
//! `smoke::stream_probe_host` or `smoke::paint_wait`. wasm32 only. Ports
//! the composition in
//! `apps/web/src/smoke/smoke.ts:50-117` over `smokeRuntimeControls.ts`,
//! `smokeTerminalRenderProbes.ts` and `smokeTerminalDomFault.ts`.

use std::rc::Rc;

use roost_client_core::ClientEvent;
use roost_client_core::store::sync_smoke::{
    TransportControl, arm_terminal_blackhole, arm_terminal_wire_delta_drop, sync_redial_report,
};
use roost_web_terminal::MAX_HELD_SCROLLBACK_ROWS;
use serde_json::{Value, json};

use super::backdoor::{Reply, SmokeBackdoor};
use super::call::SmokeCall;
use super::dom;
use super::harness::{run_flow, run_render_stress};
use super::marker_scan::scan_painted_rows;
use super::paint_wait::{
    begin_timing, finish_timing, wait_for_painted_cursor, wait_for_painted_marker,
};
use super::perf_probe::perf_probe_json;
use super::state_snapshot::{
    paint_presentation_json, painted_rows_json, redial_status_json, state_json,
};
use crate::platform::browser::perf_counters::{PerfCounters, with_perf_counters};
use crate::platform::browser::phase_marks::phase_timeline;

fn now<T: serde::Serialize>(value: T) -> Reply {
    Reply::Now(
        serde_json::to_value(value)
            .map(Some)
            .map_err(|error| error.to_string()),
    )
}

fn done() -> Reply {
    Reply::Now(Ok(None))
}

impl SmokeBackdoor {
    /// Set or release the document-visibility pin.
    ///
    /// v2's signature is `forceVisible(on)` / `forceHidden(on)`, and `false`
    /// RELEASES the pin rather than pinning the other way
    /// (`setVisibilityOverride(on ? true : null)`) — so there is no separate
    /// release member, and the 53-member smoke shape stays the one the
    /// spec-side type declares.
    ///
    /// The pin, not a document write. v2 applies the override in ONE place and
    /// requires every consumer to read visibility through `pageVisible()`; a
    /// component that read the document directly would be right in a browser
    /// and wrong under the oracle, which is the divergence that fails only in
    /// the run meant to check it. `platform::visibility` applies the override
    /// inside that one function, so every existing call site gets it unchanged.
    fn answer_flag(self: &Rc<Self>, method: &'static str, on: bool) -> Reply {
        crate::platform::visibility::pin_visible(match (method, on) {
            ("forceVisible", true) => Some(true),
            ("forceHidden", true) => Some(false),
            _ => None,
        });
        Reply::Now(Ok(None))
    }

    /// Answer one call.
    pub(super) fn answer(self: &Rc<Self>, call: SmokeCall) -> Reply {
        let this = Rc::clone(self);
        match call {
            SmokeCall::Unported { refusal } => Reply::Now(Err(refusal.to_owned())),
            SmokeCall::Bare { method } => self.answer_bare(method),
            SmokeCall::Flag { method, on } => self.answer_flag(method, on),
            SmokeCall::Session { method, session_id } => self.answer_session(method, session_id),
            SmokeCall::Input { session_id, text } => Reply::Later(Box::pin(async move {
                this.input(&session_id, &text).await.map(|()| None)
            })),
            SmokeCall::ScrollbackRange {
                method,
                session_id,
                start,
                end,
            } => {
                if method == "hasPaintedScrollbackRange" {
                    now(self
                        .panes
                        .has_painted_scrollback_range(&session_id, start, end))
                } else {
                    now(self
                        .panes
                        .painted_scrollback_range(&session_id, start, end)
                        .map(|rows| painted_rows_json(&rows)))
                }
            }
            SmokeCall::MarkerScan { session_id, prefix } => {
                let rows = dom::painted_row_texts(&session_id);
                now(scan_painted_rows(rows.iter().map(String::as_str), &prefix))
            }
            SmokeCall::WaitForPaintedMarker {
                session_id,
                marker,
                timeout_ms,
            } => Reply::Later(Box::pin(async move {
                let (proof, _) =
                    wait_for_painted_marker(&this, &session_id, &marker, timeout_ms).await?;
                Ok(Some(proof))
            })),
            SmokeCall::WaitForPaintedCursor {
                session_id,
                row,
                column,
                timeout_ms,
            } => Reply::Later(Box::pin(async move {
                let proof = wait_for_painted_cursor(&session_id, row, column, timeout_ms).await?;
                this.record_geometry_proof(&session_id, &proof);
                Ok(Some(proof))
            })),
            SmokeCall::BeginTiming { kind, session_id } => {
                Reply::Now(begin_timing(self, kind, session_id).map(Some))
            }
            SmokeCall::FinishTiming {
                timing_id,
                session_id,
                marker,
                timeout_ms,
            } => Reply::Later(Box::pin(async move {
                finish_timing(&this, &timing_id, &session_id, &marker, timeout_ms)
                    .await
                    .map(Some)
            })),
            SmokeCall::RetainedMarkerScan {
                session_id,
                prefix,
                page_rows,
            } => Reply::Later(Box::pin(async move {
                this.retained_marker_scan(&session_id, &prefix, page_rows)
                    .await
                    .map(Some)
            })),
            SmokeCall::Navigate { href } => {
                navigate_through_router(&href);
                done()
            }
            SmokeCall::SpawnShell {
                worker_fp,
                folder,
                session_id,
            } => Reply::Later(Box::pin(async move {
                this.spawn_shell_call(&worker_fp, &folder, session_id)
                    .await
                    .map(Some)
            })),
            SmokeCall::CreateWorkspace {
                worker_fp,
                folder,
                session_id,
            } => Reply::Later(Box::pin(async move {
                this.create_workspace_call(&worker_fp, &folder, &session_id)
                    .await
                    .map(Some)
            })),
            SmokeCall::RunFlow { worker_fp } => Reply::Later(Box::pin(async move {
                serde_json::to_value(run_flow(&*this, worker_fp).await)
                    .map(Some)
                    .map_err(|error| error.to_string())
            })),
            SmokeCall::RunRenderStress(options) => Reply::Later(Box::pin(async move {
                serde_json::to_value(run_render_stress(&*this, &options).await)
                    .map(Some)
                    .map_err(|error| error.to_string())
            })),
            SmokeCall::AttachmentProbe {
                session_id,
                sha256,
                size,
                filename,
            } => Reply::Later(Box::pin(async move {
                this.attachment_probe_call(session_id, sha256, size, filename)
                    .await
                    .map(Some)
            })),
            SmokeCall::DownloadWorkerFile { worker_fp, path } => {
                Reply::Later(Box::pin(async move {
                    this.download_worker_file_call(&worker_fp, &path)
                        .await
                        .map(Some)
                }))
            }
            SmokeCall::UploadAttachment(request) => self.upload_attachment_call(request),
        }
    }

    fn answer_bare(self: &Rc<Self>, method: &'static str) -> Reply {
        let control = |control| {
            self.pump
                .dispatch(ClientEvent::SyncTransportControl(control));
            done()
        };
        match method {
            "terminalInputCapture" => {
                let core = self.pump.core();
                let core = core.borrow();
                let capture = core
                    .store()
                    .input
                    .smoke_observer
                    .as_ref()
                    .map(|observer| observer.capture());
                now(capture.map(|capture| {
                    json!({
                        "batches": capture.batches.iter().map(|batch| json!({
                            "sessionId": batch.session_id,
                            "data": batch.data,
                        })).collect::<Vec<_>>(),
                        "droppedBatches": capture.dropped_batches,
                        "outcomes": {
                            "accepted": capture.outcomes.accepted,
                            "rejected": capture.outcomes.rejected,
                            "ambiguous": capture.outcomes.ambiguous,
                        },
                    })
                }))
            }
            "resetTerminalInputCapture" => {
                if let Some(observer) = self
                    .pump
                    .core()
                    .borrow_mut()
                    .store_mut()
                    .input
                    .smoke_observer
                    .as_mut()
                {
                    observer.reset_capture();
                }
                done()
            }
            "state" => now(state_json(self.pump.core().borrow().store())),
            "forceSyncMaxBackoff" => control(TransportControl::ArmMaxBackoff),
            "pauseSyncTransport" => control(TransportControl::Pause),
            "resumeSyncTransport" => control(TransportControl::Resume),
            "syncRedialStatus" => now(redial_status_json(&sync_redial_report(
                self.pump.core().borrow().store(),
            ))),
            "syncWsGeneration" => now(self.pump.sync_dial_count()),
            "phaseTimeline" => now(phase_timeline()),
            "resetPerfCounters" => {
                with_perf_counters(PerfCounters::reset);
                done()
            }
            "cleanupCreated" => {
                let this = Rc::clone(self);
                Reply::Later(Box::pin(async move {
                    serde_json::to_value(this.cleanup_created_call().await)
                        .map(Some)
                        .map_err(|error| error.to_string())
                }))
            }
            other => Reply::Now(Err(format!("__smoke.{other} is not a bare member"))),
        }
    }

    fn answer_session(self: &Rc<Self>, method: &'static str, session_id: String) -> Reply {
        let sid = session_id.as_str();
        let counts =
            |read: fn(&roost_client_core::terminal::frame_counts::FrameCounts) -> Value,
             absent: Value| {
                let core = self.pump.core();
                let core = core.borrow();
                let value = core
                    .store()
                    .terminal(sid)
                    .map_or(absent, |replica| read(&replica.frame_counts));
                Reply::Now(Ok(Some(value)))
            };
        match method {
            "paneFocused" => now(dom::pane_focus(sid)),
            "viewportText" => now(dom::viewport_text(sid)),
            "renderProbe" => now(dom::render_probe(sid)),
            "terminalDimensions" => now(dom::terminal_dimensions(sid)),
            "paintedScrollback" => now(paint_presentation_json(
                self.panes
                    .paint_presentation(sid, Some(MAX_HELD_SCROLLBACK_ROWS))
                    .as_ref(),
            )),
            "cellFrameCount" => counts(|counts| counts.frames().into(), json!(0)),
            "cellFullFrameCount" => counts(|counts| counts.full_frames().into(), json!(0)),
            "lastFullFrameSbRows" => counts(
                |counts| counts.last_full_scrollback_rows().into(),
                json!(-1),
            ),
            "cellGridEpoch" => counts(|counts| counts.grid_epoch().into(), json!("")),
            "perfProbe" => {
                let frames =
                    self.pump
                        .core()
                        .borrow()
                        .store()
                        .terminal(sid)
                        .map_or((0, 0), |replica| {
                            (
                                replica.frame_counts.frames(),
                                replica.frame_counts.full_frames(),
                            )
                        });
                now(perf_probe_json(frames.0, frames.1))
            }
            "scrollbackBackfillRequestCount" => now(self.panes.counters(sid).backfill_requests),
            "directHistoryResponseCount" => now(self.panes.counters(sid).direct_history_responses),
            "terminalBrowserSnapshot" => Reply::Now(Ok(Some(self.terminal_browser_snapshot(sid)))),
            "terminalStreamProbe" => {
                let this = Rc::clone(self);
                Reply::Later(Box::pin(async move {
                    this.terminal_stream_probe_call(&session_id).await.map(Some)
                }))
            }
            "blackholeTerminalFramesForCurrentGeneration" | "dropNextTerminalWireDelta" => {
                let core = self.pump.core();
                let mut core = core.borrow_mut();
                let armed = if method == "dropNextTerminalWireDelta" {
                    arm_terminal_wire_delta_drop(core.store_mut(), sid)
                } else {
                    arm_terminal_blackhole(core.store_mut(), sid)
                };
                if !armed {
                    tracing::warn!(target: "smoke", session_id = sid, method, "no terminal generation to fence the fault to");
                }
                done()
            }
            "dropNextCellFrame" => {
                self.arm_renderer_drop(sid);
                done()
            }
            "droppedCellFrameCount" => now(self.panes.dropped_frame_count(sid)),
            "holdTerminalDomForCurrentGeneration" => {
                Reply::Now(self.hold_terminal_dom(sid).map(|()| None))
            }
            "releaseTerminalDomHold" => {
                if self.holds.borrow_mut().release(sid) {
                    self.panes.set_dom_hold(sid, false);
                }
                done()
            }
            "trackCreatedSession" => {
                self.created.borrow_mut().track_session(sid);
                self.persist_created();
                done()
            }
            "kill" => {
                let this = Rc::clone(self);
                Reply::Later(Box::pin(async move {
                    this.kill_call(&session_id).await.map(Some)
                }))
            }
            other => Reply::Now(Err(format!("__smoke.{other} is not a session member"))),
        }
    }
}

/// `navigate(href)`: move the address bar, then let the router's own
/// `popstate` listener move the rendered path, as Back/Forward do.
pub(super) fn navigate_through_router(href: &str) {
    crate::platform::location::navigate(href);
    let Some(window) = dom::window() else {
        return;
    };
    match web_sys::PopStateEvent::new("popstate") {
        Ok(event) => {
            let _ = window.dispatch_event(&event);
        }
        Err(_) => tracing::warn!(target: "smoke", href, "popstate event refused"),
    }
    tracing::info!(target: "smoke", href, "smoke navigation");
}
