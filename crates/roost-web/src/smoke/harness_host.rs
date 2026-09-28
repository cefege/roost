//! The backdoor as the host `runFlow` and `runRenderStress` drive: worker
//! choice, spawn, route, paint and marker proofs, cleanup, and the deck-size
//! hammer. wasm32 only; the flows themselves are the native `harness`. Ports
//! the host halves of `apps/web/src/smoke/smokeHarness.ts`.

use serde_json::Value;

use super::backdoor::SmokeBackdoor;
use super::call::DEFAULT_PAINT_TIMEOUT_MS;
use super::dispatch::navigate_through_router;
use super::dom;
use super::harness::{FlowHost, StressHost};
use super::marker_scan::{SmokeMarkerScan, scan_painted_rows};
use super::paint_wait::{next_frame, random_uuid, wait_for_painted_marker};

impl FlowHost for SmokeBackdoor {
    fn workers_by_recency(&self) -> Vec<String> {
        let core = self.pump.core();
        let core = core.borrow();
        let mut workers: Vec<(&String, i64)> = core
            .store()
            .workers
            .iter()
            .map(|(fp, worker)| (fp, worker.last_seen_ms))
            .collect();
        workers.sort_by_key(|(_, last_seen_ms)| std::cmp::Reverse(*last_seen_ms));
        workers.into_iter().map(|(fp, _)| fp.clone()).collect()
    }

    fn has_worker(&self, worker_fp: &str) -> bool {
        self.pump
            .core()
            .borrow()
            .store()
            .workers
            .contains_key(worker_fp)
    }

    async fn spawn_shell(&self, worker_fp: &str, folder: &str) -> Result<String, String> {
        let spawned = self.spawn_shell_call(worker_fp, folder, None).await?;
        Ok(spawned["session_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned())
    }

    fn open_session_route(&self, session_id: &str) {
        navigate_through_router(&format!("/s/{session_id}"));
    }

    fn pane_rendered(&self, session_id: &str) -> (bool, usize) {
        let probe = dom::render_probe(session_id);
        (probe.found, probe.non_empty_rows)
    }

    async fn create_workspace(
        &self,
        worker_fp: &str,
        folder: &str,
        session_id: &str,
    ) -> Result<Value, String> {
        self.create_workspace_call(worker_fp, folder, session_id)
            .await
    }

    fn marker_nonce(&self) -> String {
        random_uuid().chars().take(8).collect()
    }

    async fn input(&self, session_id: &str, text: &str) -> Result<(), String> {
        SmokeBackdoor::input(self, session_id, text).await
    }

    async fn wait_for_painted_marker(
        &self,
        session_id: &str,
        marker: &str,
    ) -> Result<Value, String> {
        let (proof, _) =
            wait_for_painted_marker(self, session_id, marker, DEFAULT_PAINT_TIMEOUT_MS).await?;
        Ok(proof)
    }

    async fn terminal_stream_probe(&self, session_id: &str) -> Result<Value, String> {
        self.terminal_stream_probe_call(session_id).await
    }

    async fn cleanup_created(&self) -> (Value, bool) {
        let report = self.cleanup_created_call().await;
        let clean = report.errors.is_empty();
        (serde_json::to_value(report).unwrap_or(Value::Null), clean)
    }

    async fn next_frame(&self) {
        next_frame().await;
    }
}

impl StressHost for SmokeBackdoor {
    fn deck_size(&self) -> Option<(f64, f64)> {
        let deck = dom::terminal_deck()?;
        let rect = dom::rect_of(&deck);
        Some((rect.width, rect.height))
    }

    fn deck_style(&self) -> Option<String> {
        dom::terminal_deck()?.get_attribute("style")
    }

    fn set_deck_size(&self, width_px: i64, height_px: i64) {
        if let Some(deck) = dom::terminal_deck() {
            let style = deck.style();
            let _ = style.set_property("width", &format!("{width_px}px"));
            let _ = style.set_property("height", &format!("{height_px}px"));
        }
    }

    fn restore_deck_style(&self, original: Option<&str>) {
        if let Some(deck) = dom::terminal_deck() {
            let _ = match original {
                Some(style) => deck.set_attribute("style", style),
                None => deck.remove_attribute("style"),
            };
        }
    }

    fn marker_scan(&self, session_id: &str, prefix: &str) -> SmokeMarkerScan {
        let rows = dom::painted_row_texts(session_id);
        scan_painted_rows(rows.iter().map(String::as_str), prefix)
    }

    fn cell_frame_count(&self, session_id: &str) -> u64 {
        self.pump
            .core()
            .borrow()
            .store()
            .terminal(session_id)
            .map_or(0, |replica| replica.frame_counts.frames())
    }

    fn renders_cells(&self, session_id: &str) -> bool {
        dom::render_probe(session_id).found
    }

    async fn next_frame(&self) {
        next_frame().await;
    }
}
