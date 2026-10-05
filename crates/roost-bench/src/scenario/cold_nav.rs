//! `cold_nav`: navigate to the session route five times and time navigation
//! start → first painted terminal cells. The first navigation follows an HTTP
//! cache clear (`cold_nav_first_ms`); the other four reuse it
//! (`cold_nav_warm_ms`).

use std::time::Duration;

use serde_json::{Value, json};

use crate::coord::now_ms;
use crate::error::BenchError;
use crate::scenario::{Sample, SessionPage, carrier_of, wait_session_ready};

const NAVIGATIONS: usize = 5;
const PHASES_OF_INTEREST: [&str; 2] = ["terminal_mount", "first_cell_apply"];

pub async fn cold_nav(session: &SessionPage<'_>) -> Result<Vec<Sample>, BenchError> {
    let page = &session.page;
    let mut samples = Vec::with_capacity(NAVIGATIONS);
    for navigation in 0..NAVIGATIONS {
        if navigation == 0 {
            page.clear_http_cache().await?;
        }
        let navigation_started_ms = now_ms() as f64;
        page.goto(&session.session_url).await?;
        wait_session_ready(page).await?;
        let first_cells_epoch_ms: f64 = page.eval("window.__bench.firstCellsEpochMs").await?;
        let detail = json!({
            "navigation": navigation + 1,
            "carrier": carrier_of(page).await,
            "phases": phases_of_interest(page.eval::<Option<String>>("window.__bench.phase()").await?),
        });
        let metric = if navigation == 0 {
            "cold_nav_first_ms"
        } else {
            "cold_nav_warm_ms"
        };
        samples.push(Sample::new(
            metric,
            first_cells_epoch_ms - navigation_started_ms,
            detail,
        ));
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Ok(samples)
}

/// `sinceNavigationMs` of the v3 phase marks the gate doc tracks; `null` on v2,
/// which has no phase timeline.
fn phases_of_interest(timeline: Option<String>) -> Value {
    let Some(timeline) = timeline.and_then(|text| serde_json::from_str::<Value>(&text).ok()) else {
        return Value::Null;
    };
    let mut phases = serde_json::Map::new();
    if let Some(marks) = timeline.get("marks").and_then(Value::as_array) {
        for mark in marks {
            let Some(name) = mark.get("name").and_then(Value::as_str) else {
                continue;
            };
            if PHASES_OF_INTEREST.contains(&name) && !phases.contains_key(name) {
                phases.insert(
                    name.to_string(),
                    mark.get("sinceNavigationMs")
                        .cloned()
                        .unwrap_or(Value::Null),
                );
            }
        }
    }
    Value::Object(phases)
}
