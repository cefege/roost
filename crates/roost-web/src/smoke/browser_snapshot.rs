//! `terminalBrowserSnapshot(sessionId)`: the browser layer of the layered
//! terminal probe — the store's stream diagnostics, the mounted renderer's
//! watermarks and presentation, the painted history range, the last geometry
//! proof, and the pane's slot and page visibility. Native; the wasm32 adapter
//! in `smoke::stream_probe_host` reads the DOM half. Ports
//! `apps/web/src/renderer/terminalDiagSnapshot.ts:62-187`.

use std::collections::BTreeMap;

use roost_client_core::Store;
use roost_web_terminal::{
    ReaderIntent, ReaderIntentReason, ReconcileBlockReason, RendererEpochSeq,
    RendererPresentationSnapshot, RendererTerminalModeSnapshot,
};
use serde_json::{Value, json};

use super::stream_diagnostics::{DiagnosticClocks, terminal_stream_diagnostics};
use crate::components::terminal::pane_registry::PaneRenderProbe;

/// The commit this bundle was built from, under the variable every roost
/// binary is stamped with (`roost_host::build_identity`).
const BUILD_SHA: Option<&str> = option_env!("ROOST_BUILD_SHA");

/// What the mounted renderer answers, `registered: false` with no pane.
#[derive(Debug, Clone, Default)]
pub struct RendererLayer {
    /// A pane for the session is in the registry.
    pub registered: bool,
    /// Its watermarks, block reason and history anchor.
    pub probe: Option<PaneRenderProbe>,
    /// Its presentation snapshot.
    pub presentation: Option<RendererPresentationSnapshot>,
    /// The latest geometry proof recorded against this mount.
    pub last_geometry_proof: Option<Value>,
    /// The deck's `in_layout` for this pane, `None` with no mount.
    pub in_layout: Option<bool>,
    /// The deck's `surface_active` for this pane, `None` with no mount.
    pub surface_active: Option<bool>,
}

/// What the page answers about the pane's surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageLayer {
    /// The pane's scroll display is in the document.
    pub connected: bool,
    /// It has a box and no ancestor hides it; `None` when not connected.
    pub css_visible: Option<bool>,
    /// `document.visibilityState === "visible"`.
    pub document_visible: bool,
    /// The page-visibility read every terminal consumer uses.
    pub page_visible: bool,
}

/// The browser snapshot, in v2's `TerminalBrowserStreamSnapshot` shape.
pub fn terminal_browser_snapshot(
    store: &Store,
    session_id: &str,
    renderer: &RendererLayer,
    page: PageLayer,
    clocks: DiagnosticClocks,
) -> Value {
    let mut out = terminal_stream_diagnostics(store, session_id, clocks);
    let probe = renderer.probe.as_ref();
    // The RENDERER's canonical, not the store's. They are the same frame today
    // and are not the same fact: the store is what the client folded, the probe
    // is what the mount actually applied to a grid, and the specs that watch
    // them diverge are watching for exactly the moment they stop agreeing.
    let handler_canonical = probe.map_or_else(
        || json!({ "grid_epoch": null, "seq": null }),
        |probe| epoch_seq_json(&probe.canonical),
    );
    let anchor = probe.and_then(|probe| probe.backfill_anchor.as_ref());
    let entries = [
        ("session_id", json!(session_id)),
        ("captured_at_ms", json!(clocks.epoch_ms)),
        ("build", json!({ "git_sha": BUILD_SHA })),
        ("handler_canonical", handler_canonical),
        (
            "dom_reconciled",
            probe.map_or_else(
                || json!({ "grid_epoch": null, "seq": null }),
                |probe| epoch_seq_json(&probe.reconciled),
            ),
        ),
        (
            "reconcile_block_reason",
            probe.map_or(Value::Null, |probe| {
                block_reason(probe.reconcile_block_reason).map_or(Value::Null, Value::from)
            }),
        ),
        (
            "presentation",
            renderer
                .presentation
                .as_ref()
                .map_or(Value::Null, presentation_json),
        ),
        (
            "history",
            json!({
                "grid_epoch": anchor.map(|anchor| anchor.grid_epoch.clone()),
                "sb_base": anchor.map(|anchor| anchor.sb_base),
                "total": anchor.map(|anchor| anchor.total),
                "cols": anchor.map(|anchor| anchor.cols),
                "rows_held": probe.map_or(0, |probe| probe.painted_scrollback_rows),
                "floor": null,
            }),
        ),
        (
            "last_geometry_proof",
            renderer.last_geometry_proof.clone().unwrap_or(Value::Null),
        ),
        (
            "slot",
            json!({
                "registered": renderer.registered,
                "connected": renderer.registered && page.connected,
                "in_layout": renderer.in_layout,
                "surface_active": renderer.surface_active,
                "css_visible": if renderer.registered { page.css_visible } else { None },
            }),
        ),
        (
            "visibility",
            json!({
                "document_visible": page.document_visible,
                "page_visible": page.page_visible,
            }),
        ),
    ];
    for (key, value) in entries {
        out.insert(key.to_owned(), value);
    }
    Value::Object(out)
}

/// `RendererPresentationSnapshot` (`cellRendererPresentation.ts:114-138`).
pub fn presentation_json(snapshot: &RendererPresentationSnapshot) -> Value {
    json!({
        "captured_at_ms": snapshot.captured_at_ms,
        "canonical": epoch_seq_json(&snapshot.canonical),
        "reconciled": epoch_seq_json(&snapshot.reconciled),
        "reader_intent": match snapshot.reader_intent {
            ReaderIntent::Live => "live",
            ReaderIntent::Reading => "reading",
        },
        "reader_reason": snapshot.reader_reason.map(reader_reason),
        "hold_mask": {
            "selection": snapshot.hold_mask_selection,
            "link": snapshot.hold_mask_link,
        },
        "rows": { "canonical": snapshot.canonical_rows, "dom": snapshot.dom_rows },
        "mode": {
            "canonical": snapshot.canonical_mode.as_ref().map(mode_json),
            "reconciled": snapshot.reconciled_mode.as_ref().map(mode_json),
        },
        "cursor": {
            "canonical": snapshot.canonical_cursor.map(|(visible, row, column)| {
                json!({ "visible": visible, "row": row, "column": column })
            }),
            "dom": {
                "visible": snapshot.painted_cursor_visible,
                "row": snapshot.painted_cursor_row,
                "column": snapshot.painted_cursor_col,
                "connected": snapshot.cursor_connected,
            },
        },
        "cols": { "canonical": snapshot.canonical_cols, "dom": snapshot.painted_cols },
        "at_bottom": snapshot.at_bottom,
        "follows_bottom": snapshot.follows_bottom,
    })
}

fn epoch_seq_json(watermark: &RendererEpochSeq) -> Value {
    json!({ "grid_epoch": watermark.grid_epoch, "seq": watermark.seq })
}

fn mode_json(mode: &RendererTerminalModeSnapshot) -> Value {
    json!({
        "alt_screen": mode.alt_screen,
        "cursor_keys_app": mode.cursor_keys_app,
        "bracketed_paste": mode.bracketed_paste,
    })
}

fn reader_reason(reason: ReaderIntentReason) -> &'static str {
    match reason {
        ReaderIntentReason::NativeScroll => "native_scroll",
        ReaderIntentReason::Wheel => "wheel",
        ReaderIntentReason::Touch => "touch",
        ReaderIntentReason::Selection => "selection",
        ReaderIntentReason::Find => "find",
    }
}

/// v2's `ReconcileBlockReason`, `None` being its `null`.
fn block_reason(reason: ReconcileBlockReason) -> Option<&'static str> {
    Some(match reason {
        ReconcileBlockReason::ReaderPendingFrame => "reader_pending_frame",
        ReconcileBlockReason::SelectionHold => "selection_hold",
        ReconcileBlockReason::LinkHold => "link_hold",
        ReconcileBlockReason::SelectionAndLinkHold => "selection_and_link_hold",
        ReconcileBlockReason::PendingRender => "pending_render",
        ReconcileBlockReason::NotReconciled => "not_reconciled",
        ReconcileBlockReason::None => return None,
    })
}

/// The latest successful geometry proof per session, owned by the mount it
/// was proven on: v2 keeps it on the renderer registry entry, so a remount
/// starts with none (`recordTerminalGeometryProof`).
#[derive(Debug, Default)]
pub struct GeometryProofs {
    by_session: BTreeMap<String, (u64, Value)>,
}

impl GeometryProofs {
    /// Keep `proof` for the mount now painting `session_id`. A proof naming
    /// another session, or a session with no mounted pane, is not kept.
    pub fn record(&mut self, session_id: &str, mount_id: Option<u64>, proof: &Value) {
        let Some(mount_id) = mount_id else {
            return;
        };
        if proof.get("sessionId").and_then(Value::as_str) != Some(session_id) {
            return;
        }
        self.by_session
            .insert(session_id.to_owned(), (mount_id, proof.clone()));
    }

    /// The proof recorded against the current mount.
    pub fn latest(&self, session_id: &str, mount_id: Option<u64>) -> Option<Value> {
        let (recorded_mount, proof) = self.by_session.get(session_id)?;
        (Some(*recorded_mount) == mount_id).then(|| proof.clone())
    }
}
