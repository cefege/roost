//! The JSON answers that are pure reads of the store and the pane registry:
//! `state()` in v2's `rootStore` record shape, `syncRedialStatus()`, and the
//! painted-scrollback presentation. Native; called by `smoke::dispatch`. Ports
//! `apps/web/src/smoke/smokeRuntimeControls.ts:59-66` and the
//! `RendererPaintPresentation` shape of `apps/web/src/renderer/cellRendererPresentation.ts:102-112`.

use roost_client_core::store::PairRequest;
use roost_client_core::store::sync_smoke::SyncRedialReport;
use roost_client_core::Store;
use roost_web_terminal::{PaintedRowText, RendererPaintPresentation};
use serde::Serialize;
use serde_json::{Map, Value, json};

/// `state()`: sessions, workspaces and workers keyed by id, plus pair requests.
pub fn state_json(store: &Store) -> Value {
    let sessions = keyed(
        store
            .sessions
            .sessions()
            .iter()
            .map(|(id, session)| (id.as_str().to_owned(), session)),
    );
    let workspaces = keyed(store.workspaces.iter().map(|(id, row)| (id.clone(), row)));
    let workers = keyed(store.workers.iter().map(|(fp, row)| (fp.clone(), row)));
    let pair_requests: Map<String, Value> = store
        .pair_requests
        .iter()
        .map(|(id, request)| (id.clone(), pair_request_json(request)))
        .collect();
    json!({
        "sessions": sessions,
        "workspaces": workspaces,
        "workers": workers,
        "pair_requests": pair_requests,
    })
}

/// Rows keyed by id; a row that does not serialize is left out rather than
/// published as `null`, which a `state().workers[fp]` wait would read as absent
/// anyway.
fn keyed<'a, T: Serialize + 'a>(rows: impl Iterator<Item = (String, &'a T)>) -> Map<String, Value> {
    rows.filter_map(|(id, row)| serde_json::to_value(row).ok().map(|value| (id, value)))
        .collect()
}

/// v2's `PairRequest` record (`apps/web/src/store/root.ts:20-36`).
pub fn pair_request_json(request: &PairRequest) -> Value {
    json!({
        "ephemeral_id": request.ephemeral_id,
        "label": request.label,
        "created_at_ms": request.created_at_ms,
        "userAgent": request.user_agent,
        "clientBrowser": request.client_browser,
        "clientOs": request.client_os,
        "clientDeviceType": request.client_device_type,
        "sourceIp": request.source_ip,
        "countryCode": request.country_code,
        "region": request.region,
        "city": request.city,
        "edgeIdentityProvider": request.edge_identity_provider,
        "edgeIdentity": request.edge_identity,
        "edgeIdentityVerified": request.edge_identity_verified,
        "expiresAtMs": request.expires_at_ms,
    })
}

/// `syncRedialStatus()` (`SyncRedialStatus` in `apps/web/src/store/sync-redial.ts:26-35`).
pub fn redial_status_json(report: &SyncRedialReport) -> Value {
    json!({
        "failures": report.failures,
        "nextDelayMs": report.next_delay_ms,
        "hiddenParked": report.hidden_parked,
        "liveness": report.liveness.as_str(),
    })
}

/// `paintedScrollback()`: the presentation, or the empty one with no pane.
pub fn paint_presentation_json(presentation: Option<&RendererPaintPresentation>) -> Value {
    let Some(presentation) = presentation else {
        return json!({ "rows": [], "headSpacerPx": 0, "tailGapPx": 0, "readerAnchor": null });
    };
    json!({
        "rows": painted_rows_json(&presentation.rows),
        "headSpacerPx": presentation.head_spacer_px,
        "tailGapPx": presentation.tail_gap_px,
        "readerAnchor": presentation
            .reader_anchor
            .as_ref()
            .map(|anchor| json!({ "row": anchor.row, "offsetPx": anchor.offset_px })),
    })
}

/// `[{ index, text }]` rows.
pub fn painted_rows_json(rows: &[PaintedRowText]) -> Value {
    Value::Array(
        rows.iter()
            .map(|row| json!({ "index": row.index, "text": row.text }))
            .collect(),
    )
}
