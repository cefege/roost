//! `cold_nav`: navigate to the session route five times and time navigation
//! start → first painted terminal cells. The first navigation follows an HTTP
//! cache clear (`cold_nav_first_ms`); the other four reuse it
//! (`cold_nav_warm_ms`). Every navigation also splits its boot: the
//! document's DOMContentLoaded (`boot_dcl_ms`, both stacks) and, from v3's
//! phase marks and the wasm's resource timing, wasm fetch → module start →
//! terminal mount → first applied cells (`boot_*_ms`).

use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::coord::now_ms;
use crate::error::BenchError;
use crate::scenario::{Sample, SessionPage, carrier_of, wait_session_ready};

const NAVIGATIONS: usize = 5;

pub async fn cold_nav(session: &SessionPage<'_>) -> Result<Vec<Sample>, BenchError> {
    let page = &session.page;
    let mut samples = Vec::with_capacity(NAVIGATIONS * 6);
    for navigation in 0..NAVIGATIONS {
        if navigation == 0 {
            page.clear_http_cache().await?;
        }
        let navigation_started_ms = now_ms() as f64;
        page.goto(&session.session_url).await?;
        wait_session_ready(page).await?;
        let first_cells_epoch_ms: f64 = page.eval("window.__bench.firstCellsEpochMs").await?;
        let phases = all_phases(
            page.eval::<Option<String>>("window.__bench.phase()")
                .await?,
        );
        let boot: Value = page.eval("window.__bench.bootTiming()").await?;
        let metric = if navigation == 0 {
            "cold_nav_first_ms"
        } else {
            "cold_nav_warm_ms"
        };
        let detail = json!({
            "navigation": navigation + 1,
            "carrier": carrier_of(page).await,
            "phases": phases,
            "boot": boot,
        });
        samples.extend(boot_samples(&detail));
        samples.push(Sample::new(
            metric,
            first_cells_epoch_ms - navigation_started_ms,
            detail,
        ));
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Ok(samples)
}

/// Every phase mark's first `sinceNavigationMs`, by name; `null` on v2, which
/// has no phase timeline.
fn all_phases(timeline: Option<String>) -> Value {
    let Some(timeline) = timeline.and_then(|text| serde_json::from_str::<Value>(&text).ok()) else {
        return Value::Null;
    };
    let mut phases = Map::new();
    for mark in timeline
        .get("marks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(name) = mark.get("name").and_then(Value::as_str) else {
            continue;
        };
        if !phases.contains_key(name) {
            phases.insert(
                name.to_string(),
                mark.get("sinceNavigationMs")
                    .cloned()
                    .unwrap_or(Value::Null),
            );
        }
    }
    Value::Object(phases)
}

/// The boot split of one navigation. A stage whose two ends were not both
/// recorded is left out rather than reported as zero.
fn boot_samples(detail: &Value) -> Vec<Sample> {
    let at = |pointer: &str| detail.pointer(pointer).and_then(Value::as_f64);
    let navigation = json!({ "navigation": detail.get("navigation") });
    let stages = [
        ("boot_dcl_ms", Some(0.0), at("/boot/nav/dcl")),
        (
            "boot_wasm_fetch_ms",
            at("/boot/wasm/start"),
            at("/boot/wasm/end"),
        ),
        (
            "boot_wasm_to_main_ms",
            at("/boot/wasm/end"),
            at("/phases/module_start"),
        ),
        (
            "boot_main_to_mount_ms",
            at("/phases/module_start"),
            at("/phases/terminal_mount"),
        ),
        (
            "boot_mount_to_cells_ms",
            at("/phases/terminal_mount"),
            at("/phases/first_cell_apply"),
        ),
    ];
    stages
        .into_iter()
        .filter_map(|(metric, from, to)| Some(Sample::new(metric, to? - from?, navigation.clone())))
        .collect()
}
