//! What the UI bridge needs from the document around it, and the browser's
//! answer to all of it.
//!
//! Ports the `io` object `apps/web/src/components/UiBridge.tsx` closed over
//! `useNavigate`/`useLocation`, and the `targetDependencies` of
//! `apps/web/src/lib/uiLayoutApply.ts`. The bridge is a machine with no opinion
//! about the address bar, the transport or the viewport, so every one of those
//! arrives through [`UiBridgeHost`] — which is also what lets a native test
//! drive the whole bridge without a document.

use dioxus::prelude::*;
use roost_client_core::SyncCommand;
use roost_client_core::client::rpc::calls::ui_state::UiReportState;
use roost_client_core::client::ui_command::UiStateReport;
use roost_client_core::client::ui_state::LayoutApplyResult;

use crate::pump::Pump;
use crate::router_state::navigate_path;

/// What the shell is showing right now, published for the tick that runs
/// outside a render.
///
/// A plain value rather than a `Signal`, because the sweep listener runs
/// outside every Dioxus scope and a subscriber read there would either
/// subscribe nothing or panic. The component writes this during render, which
/// is the one place the route and the size class are readable.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShellFacts {
    /// The path the router renders.
    pub path: String,
    /// Whether the host paints one pane.
    pub compact: bool,
}

/// The document-side operations the bridge performs, as one trait.
///
/// Two implementations exist and both are complete: [`BrowserBridgeHost`] for
/// the document, and a recorder in `crates/roost-web/tests/ui_bridge.rs` that
/// proves what went out. A third would be a second answer to "where does a
/// report go".
pub trait UiBridgeHost {
    /// Move the address bar and the rendered path together, exactly as an
    /// in-app link does.
    fn navigate(&mut self, path: &str);
    /// Publish this tab's report through the coordinator client.
    fn send_report(&mut self, report: UiStateReport);
    /// Answer an acknowledged apply on the Sync socket.
    fn send_apply_result(&mut self, result: LayoutApplyResult);
    /// Whether this document holds a live Sync socket right now.
    fn sync_socket_is_open(&self) -> bool;
}

/// The bridge's host in a browser document.
#[derive(Clone)]
pub struct BrowserBridgeHost {
    pump: Pump,
    path: Signal<String>,
}

impl std::fmt::Debug for BrowserBridgeHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserBridgeHost")
            .finish_non_exhaustive()
    }
}

impl BrowserBridgeHost {
    /// A host that navigates through the router's path signal.
    pub fn new(pump: Pump, path: Signal<String>) -> Self {
        Self { pump, path }
    }
}

impl UiBridgeHost for BrowserBridgeHost {
    fn navigate(&mut self, path: &str) {
        navigate_path(self.path, path.to_owned());
    }

    fn send_report(&mut self, report: UiStateReport) {
        // The tab fence is not passed here. `CoordRpc`'s transport already
        // stamps `x-roost-tab-id` with the id `pump::boot` claimed, and the
        // report's own `tab_id` is that same id, so a second header or a
        // second claim would be a second answer to "which tab is this".
        let pump = self.pump.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let request = UiReportState { report };
            if let Err(error) = pump.rpc().call(&request).await {
                tracing::warn!(target: "ui_cc", %error, "ui state report was refused");
            }
        });
    }

    fn send_apply_result(&mut self, result: LayoutApplyResult) {
        self.pump
            .send_sync_command(SyncCommand::UiApplyLayoutResult(result));
    }

    fn sync_socket_is_open(&self) -> bool {
        self.pump.sync_socket_is_open()
    }
}
