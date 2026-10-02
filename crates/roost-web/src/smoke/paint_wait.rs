//! The paint-proof loops and the page clocks they run on: animation frames,
//! timeouts, `performance.now()`, the marker and cursor geometry reads, and the
//! trusted-keydown listener of a `trusted_key` timing. wasm32 only; the
//! geometry rules are `smoke::paint_proof`, the ledger `smoke::timing`. Ports
//! `apps/web/src/smoke/smokeHarness.ts:12-29,160-522`.

use std::rc::Rc;

use js_sys::Promise;
use serde_json::{Value, json};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Element, KeyboardEvent, Node};

use super::backdoor::SmokeBackdoor;
use super::call::TimingKind;
use super::dom;
use super::paint_proof::{
    CursorProof, MarkerProof, RectSnapshot, cursor_aligned, dataset_coordinate,
};
use super::timing::{timing_result, unknown_timing};
use crate::platform::browser::phase_marks::{PhaseName, mark_phase};

/// Resolve on the next animation frame.
pub(super) async fn next_frame() {
    let promise = Promise::new(&mut |resolve, _reject| {
        let scheduled =
            dom::window().is_some_and(|window| window.request_animation_frame(&resolve).is_ok());
        if !scheduled {
            let _ = resolve.call0(&JsValue::NULL);
        }
    });
    let _ = JsFuture::from(promise).await;
}

/// Resolve after `delay_ms`.
pub(super) async fn sleep_ms(delay_ms: i32) {
    let promise = Promise::new(&mut |resolve, _reject| {
        let scheduled = dom::window().is_some_and(|window| {
            window
                .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, delay_ms)
                .is_ok()
        });
        if !scheduled {
            let _ = resolve.call0(&JsValue::NULL);
        }
    });
    let _ = JsFuture::from(promise).await;
}

/// `performance.now()`.
pub(super) fn now_ms() -> f64 {
    dom::window()
        .and_then(|window| window.performance())
        .map_or(0.0, |clock| clock.now())
}

/// `performance.timeOrigin`.
pub(super) fn time_origin_ms() -> f64 {
    dom::window()
        .and_then(|window| window.performance())
        .map_or(0.0, |clock| clock.time_origin())
}

/// `crypto.randomUUID()`.
pub(super) fn random_uuid() -> String {
    dom::window()
        .and_then(|window| window.crypto().ok())
        .map(|crypto| crypto.random_uuid())
        .unwrap_or_else(|| format!("{:016x}", js_sys::Math::random().to_bits()))
}

struct MarkerGeometry {
    row_text: String,
    marker: RectSnapshot,
    terminal: RectSnapshot,
    viewport: RectSnapshot,
    row: Element,
}

fn find_painted_marker(session_id: &str, marker: &str) -> Option<MarkerGeometry> {
    if marker.is_empty() {
        return None;
    }
    let slot = dom::slot(session_id)?;
    let terminal = dom::grid_in(&slot)?;
    if !slot.is_connected() || !terminal.is_connected() {
        return None;
    }
    if !dom::visibly_styled(&terminal) || !dom::visibly_styled(&slot) {
        return None;
    }
    let terminal_rect = dom::rect_of(&terminal);
    let viewport = dom::visual_viewport_rect();
    if !terminal_rect.intersects(&viewport) {
        return None;
    }
    dom::all(&terminal, ".cell-row")
        .into_iter()
        .find_map(|row| {
            let row_text = dom::text_of(&row);
            if !row_text.contains(marker) || !dom::visibly_styled(&row) {
                return None;
            }
            let (marker_rect, start) = dom::marker_rect(&row, marker)?;
            let shown = dom::visibly_styled(&start)
                && marker_rect.intersects(&terminal_rect)
                && marker_rect.intersects(&viewport);
            shown.then_some(MarkerGeometry {
                row_text,
                marker: marker_rect,
                terminal: terminal_rect,
                viewport,
                row,
            })
        })
}

/// `waitForPaintedMarker`: the proof JSON and the monotonic time it was taken.
pub(super) async fn wait_for_painted_marker(
    backdoor: &SmokeBackdoor,
    session_id: &str,
    marker: &str,
    timeout_ms: f64,
) -> Result<(Value, f64), String> {
    let deadline = now_ms() + timeout_ms;
    while now_ms() <= deadline {
        if let Some(first) = find_painted_marker(session_id, marker) {
            next_frame().await;
            next_frame().await;
            if let Some(confirmed) = find_painted_marker(session_id, marker)
                && confirmed.row.is_same_node(Some(&first.row))
                && confirmed.marker.stable_with(&first.marker)
                && confirmed.terminal.stable_with(&first.terminal)
                && confirmed.viewport.stable_with(&first.viewport)
            {
                let monotonic_ms = now_ms();
                let proof = MarkerProof {
                    proof_kind: "marker",
                    session_id: session_id.to_owned(),
                    marker: marker.to_owned(),
                    monotonic_ms,
                    epoch_ms: time_origin_ms() + monotonic_ms,
                    row_text: confirmed.row_text,
                    marker_rect: confirmed.marker,
                    terminal_rect: confirmed.terminal,
                    visual_viewport_rect: confirmed.viewport,
                    frames: 2,
                };
                tracing::info!(target: "smoke", session_id, "marker presented");
                mark_phase(
                    PhaseName::MarkerPresented,
                    &[
                        ("sessionId", json!(session_id)),
                        (
                            "marker",
                            json!(marker.chars().take(160).collect::<String>()),
                        ),
                        ("markerWidth", json!(proof.marker_rect.width)),
                        ("markerHeight", json!(proof.marker_rect.height)),
                    ],
                );
                let proof = serde_json::to_value(proof).map_err(|error| error.to_string())?;
                backdoor.record_geometry_proof(session_id, &proof);
                return Ok((proof, monotonic_ms));
            }
        }
        next_frame().await;
    }
    Err(backdoor.marker_timeout_message(session_id, marker, timeout_ms))
}

struct CursorGeometry {
    row: u32,
    column: u32,
    raw: RectSnapshot,
    rect: RectSnapshot,
    terminal_clip: RectSnapshot,
    viewport: RectSnapshot,
    node: Element,
}

fn find_painted_cursor(
    session_id: &str,
    row: Option<u32>,
    column: Option<u32>,
) -> Option<CursorGeometry> {
    let slot = dom::slot(session_id)?;
    let terminal = dom::grid_in(&slot)?;
    let viewport = terminal.query_selector(".cell-viewport").ok()??;
    let cursor = viewport.query_selector(".cell-cursor").ok()??;
    let nodes: [&Node; 4] = [&slot, &terminal, &viewport, &cursor];
    if nodes.iter().any(|node| !node.is_connected())
        || !cursor
            .parent_element()
            .is_some_and(|parent| parent.is_same_node(Some(&viewport)))
        || !dom::cursor_paintable(&cursor, &terminal)
    {
        return None;
    }
    let at_row = dataset_coordinate(cursor.get_attribute("data-row").as_deref())?;
    let at_column = dataset_coordinate(cursor.get_attribute("data-column").as_deref())?;
    if row.is_some_and(|row| row != at_row) || column.is_some_and(|column| column != at_column) {
        return None;
    }
    let visual = dom::visual_viewport_rect();
    let terminal_clip = dom::rect_of(&terminal).clipped_to(&visual)?;
    let raw = dom::rect_of(&cursor);
    let rect = raw.clipped_to(&terminal_clip)?;
    let row_element = dom::all(&viewport, ".cell-row")
        .into_iter()
        .nth(at_row as usize)?;
    if !dom::visibly_styled(&row_element)
        || !cursor_aligned(&raw, &dom::rect_of(&row_element), at_column)
    {
        return None;
    }
    Some(CursorGeometry {
        row: at_row,
        column: at_column,
        raw,
        rect,
        terminal_clip,
        viewport: visual,
        node: cursor,
    })
}

/// `waitForPaintedCursor`.
pub(super) async fn wait_for_painted_cursor(
    session_id: &str,
    row: Option<u32>,
    column: Option<u32>,
    timeout_ms: f64,
) -> Result<Value, String> {
    let deadline = now_ms() + timeout_ms;
    while now_ms() <= deadline {
        if let Some(first) = find_painted_cursor(session_id, row, column) {
            next_frame().await;
            next_frame().await;
            if let Some(confirmed) = find_painted_cursor(session_id, row, column)
                && confirmed.node.is_same_node(Some(&first.node))
                && (confirmed.row, confirmed.column) == (first.row, first.column)
                && confirmed.rect.stable_with(&first.rect)
                && confirmed.raw.stable_with(&first.raw)
                && confirmed.terminal_clip.stable_with(&first.terminal_clip)
                && confirmed.viewport.stable_with(&first.viewport)
            {
                let monotonic_ms = now_ms();
                tracing::info!(target: "smoke", session_id, row = confirmed.row, "cursor presented");
                let proof = CursorProof {
                    proof_kind: "cursor",
                    session_id: session_id.to_owned(),
                    row: confirmed.row,
                    column: confirmed.column,
                    monotonic_ms,
                    epoch_ms: time_origin_ms() + monotonic_ms,
                    rect: confirmed.rect,
                    terminal_clip: confirmed.terminal_clip,
                    visual_viewport: confirmed.viewport,
                    frames: 2,
                };
                mark_phase(
                    PhaseName::CursorPresented,
                    &[
                        ("sessionId", json!(session_id)),
                        ("row", json!(proof.row)),
                        ("column", json!(proof.column)),
                        ("cursorWidth", json!(proof.rect.width)),
                        ("cursorHeight", json!(proof.rect.height)),
                    ],
                );
                return serde_json::to_value(proof).map_err(|error| error.to_string());
            }
        }
        next_frame().await;
    }
    let mut expected = serde_json::Map::new();
    if let Some(row) = row {
        expected.insert("row".to_owned(), row.into());
    }
    if let Some(column) = column {
        expected.insert("column".to_owned(), column.into());
    }
    Err(format!(
        "cursor presentation geometry was not proven within {timeout_ms}ms: {session_id} {}",
        Value::Object(expected)
    ))
}

/// `beginTerminalTiming`: the new timing's id.
pub(super) fn begin_timing(
    backdoor: &Rc<SmokeBackdoor>,
    kind: TimingKind,
    session_id: Option<String>,
) -> Result<Value, String> {
    let slot = match (kind, &session_id) {
        (TimingKind::TrustedKey, Some(session)) => {
            Some(dom::slot(session).ok_or_else(|| {
                format!("terminal slot missing for trusted_key timing: {session}")
            })?)
        }
        _ => None,
    };
    let id = random_uuid();
    let evicted =
        backdoor
            .timings
            .borrow_mut()
            .begin(&id, kind, session_id, now_ms(), time_origin_ms())?;
    if let Some(evicted) = evicted {
        backdoor.drop_timing_listener(&evicted);
    }
    if let Some(slot) = slot {
        let listening = Rc::downgrade(backdoor);
        let timing_id = id.clone();
        let listener = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
            let inside = event
                .target()
                .and_then(|target| target.dyn_into::<Node>().ok())
                .is_some_and(|target| slot.contains(Some(&target)));
            if let (true, true, Some(backdoor)) = (event.is_trusted(), inside, listening.upgrade())
            {
                backdoor.timings.borrow_mut().note_trusted_key(
                    &timing_id,
                    now_ms(),
                    time_origin_ms(),
                );
            }
        });
        backdoor.install_timing_listener(&id, listener);
    }
    Ok(Value::String(id))
}

/// `finishTerminalTiming`.
pub(super) async fn finish_timing(
    backdoor: &SmokeBackdoor,
    timing_id: &str,
    session_id: &str,
    marker: &str,
    timeout_ms: f64,
) -> Result<Value, String> {
    let begun = backdoor
        .timings
        .borrow()
        .get(timing_id)
        .ok_or_else(|| unknown_timing(timing_id))?;
    let painted = wait_for_painted_marker(backdoor, session_id, marker, timeout_ms).await;
    let timing = backdoor
        .timings
        .borrow_mut()
        .take(timing_id)
        .unwrap_or(begun);
    backdoor.drop_timing_listener(timing_id);
    let (proof, proof_ms) = painted?;
    timing_result(&timing, session_id, proof, proof_ms)
}
