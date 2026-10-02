//! `window.__roostPredictDebug()`'s answer under v2's `_debug()` keys, which
//! the predictive-echo oracle reads for its reset accounting. Pins
//! `smoke::predict_debug` (v2 `apps/web/src/renderer/predictiveEcho.ts`).
#![cfg(feature = "smoke")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::client::predictive_echo::report::{EchoDebug, ResetReason};
use roost_client_core::store::prefs::PredictMode;
use roost_web::smoke::predict_debug::predict_debug_json;
use serde_json::json;

fn debug(predicted_cursor_col: Option<u32>, last_reset: Option<ResetReason>) -> EchoDebug {
    EchoDebug {
        total: 3,
        visible: 2,
        srtt_ms: 41.5,
        confirmed_epoch: 4,
        prediction_epoch: 5,
        mode: PredictMode::Always,
        predicted_cursor_col,
        reset_count: 7,
        cleared_count: 1,
        last_reset,
    }
}

#[test]
fn the_state_is_reported_under_v2_keys() {
    assert_eq!(
        predict_debug_json(&debug(Some(12), Some(ResetReason::Cleared))),
        json!({
            "total": 3,
            "visible": 2,
            "srtt": 41.5,
            "confirmedEpoch": 4,
            "predictionEpoch": 5,
            "mode": PredictMode::Always.as_str(),
            "predCursorCol": 12,
            "resetCount": 7,
            "clearedCount": 1,
            "lastReset": "cleared",
        })
    );
}

#[test]
fn a_caret_that_does_not_lead_is_minus_one_and_no_reset_is_null() {
    let value = predict_debug_json(&debug(None, None));
    assert_eq!(value["predCursorCol"], json!(-1));
    assert_eq!(value["lastReset"], json!(null));
}
