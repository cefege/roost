//! The wasm32 half of `terminalBrowserSnapshot` and `terminalStreamProbe`:
//! reading the pane registry, the store, the page and the two clocks into
//! `smoke::browser_snapshot`, the `DiagSnapshot` round trip whose answer
//! `smoke::stream_probe` normalizes, and the geometry-proof ledger the paint
//! proofs record into. Ports the adapters of
//! `apps/web/src/smoke/smokeTerminalStreamProbe.ts:23-43` and
//! `apps/web/src/renderer/terminalDiagSnapshot.ts:107-128`.

use roost_client_core::Clock as _;
use roost_client_core::client::rpc::calls::diagnostics::DiagSnapshot;
use serde_json::Value;

use super::backdoor::SmokeBackdoor;
use super::browser_snapshot::{PageLayer, RendererLayer, terminal_browser_snapshot};
use super::dom;
use super::stream_diagnostics::DiagnosticClocks;
use super::stream_probe::{normalize_terminal_stream_probe, parse_coordinator_snapshot};
use crate::components::terminal::dom::page_visible;
use crate::platform::BrowserClock;

impl SmokeBackdoor {
    /// `terminalBrowserSnapshot(sessionId)`.
    pub(super) fn terminal_browser_snapshot(&self, session_id: &str) -> Value {
        let mount_id = self.panes.mount_id(session_id);
        let renderer = RendererLayer {
            registered: mount_id.is_some(),
            probe: self.panes.render_probe(session_id),
            presentation: self.panes.presentation_snapshot(session_id),
            last_geometry_proof: self.geometry_proofs.borrow().latest(session_id, mount_id),
        };
        let clocks = DiagnosticClocks {
            monotonic_ms: BrowserClock::new().now_ms(),
            epoch_ms: js_sys::Date::now(),
        };
        let core = self.pump.core();
        let core = core.borrow();
        terminal_browser_snapshot(
            core.store(),
            session_id,
            &renderer,
            page_layer(session_id),
            clocks,
        )
    }

    /// `terminalStreamProbe(sessionId)`: the browser layer rides the request
    /// as the coordinator's opaque SPA payload.
    pub(super) async fn terminal_stream_probe_call(
        &self,
        session_id: &str,
    ) -> Result<Value, String> {
        let browser = self.terminal_browser_snapshot(session_id);
        let request = DiagSnapshot {
            spa_state_json: browser.to_string(),
        };
        let snapshot_json = self
            .pump
            .rpc()
            .call(&request)
            .await
            .map_err(|error| error.to_string())?;
        let raw = parse_coordinator_snapshot(&snapshot_json)?;
        normalize_terminal_stream_probe(session_id, browser, &raw)
    }

    /// Keep a successful marker or cursor proof for the mount it proved.
    pub(super) fn record_geometry_proof(&self, session_id: &str, proof: &Value) {
        let mount_id = self.panes.mount_id(session_id);
        self.geometry_proofs
            .borrow_mut()
            .record(session_id, mount_id, proof);
    }
}

/// The pane's scroll display as v2's owner source reads it: in the document,
/// with a box, and hidden by no ancestor's computed style.
fn page_layer(session_id: &str) -> PageLayer {
    let display = dom::slot(session_id).and_then(|slot| dom::grid_in(&slot));
    let connected = display
        .as_ref()
        .is_some_and(|display| display.is_connected());
    let css_visible = display.filter(|_| connected).map(|display| {
        let rect = dom::rect_of(&display);
        rect.width > 0.0 && rect.height > 0.0 && dom::visibly_styled(&display)
    });
    let document_visible = dom::document().is_some_and(|document| !document.hidden());
    PageLayer {
        connected,
        css_visible,
        document_visible,
        page_visible: page_visible(),
    }
}
