//! `window.__roostPredictDebug()`: the mounted pane's predictive-echo state
//! under v2's keys, for the oracle's reset accounting. The pane's echo attach
//! installs one per mount — the last mounted pane wins — and its detach
//! removes it only while it is still the installed one. Ports the smoke hook
//! of `apps/web/src/components/terminal/cell-terminal-renderer.ts` and
//! `PredictiveEcho._debug()` of `apps/web/src/renderer/predictiveEcho.ts`.

use roost_client_core::client::predictive_echo::report::EchoDebug;
use serde_json::{Value, json};

/// The state under v2's `_debug()` keys; a caret that does not lead is `-1`.
pub fn predict_debug_json(debug: &EchoDebug) -> Value {
    json!({
        "total": debug.total,
        "visible": debug.visible,
        "srtt": debug.srtt_ms,
        "confirmedEpoch": debug.confirmed_epoch,
        "predictionEpoch": debug.prediction_epoch,
        "mode": debug.mode.as_str(),
        "predCursorCol": debug.predicted_cursor_col.map_or(-1, i64::from),
        "resetCount": debug.reset_count,
        "clearedCount": debug.cleared_count,
        "lastReset": debug.last_reset.map(|reason| reason.as_str()),
    })
}

#[cfg(target_arch = "wasm32")]
pub use hook::PredictDebugHook;

#[cfg(target_arch = "wasm32")]
mod hook {
    use js_sys::{JSON, Object, Reflect};
    use roost_client_core::KeyValueStore as _;
    use roost_client_core::client::predictive_echo::report::EchoDebug;
    use wasm_bindgen::JsValue;
    use wasm_bindgen::closure::Closure;

    use super::predict_debug_json;
    use crate::platform::LocalStorageKeyValueStore;

    const GLOBAL: &str = "__roostPredictDebug";

    /// One pane's installed reader. Dropping it uninstalls it if no later
    /// mount replaced it, so the global never calls into a dropped closure.
    pub struct PredictDebugHook {
        read: Closure<dyn Fn() -> JsValue>,
    }

    impl std::fmt::Debug for PredictDebugHook {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("PredictDebugHook")
                .finish_non_exhaustive()
        }
    }

    impl PredictDebugHook {
        /// Install `read_debug` as the global when this document opted into
        /// the smoke backdoor. `None` answers `null`, as a pane without a
        /// predictor does.
        pub fn install(read_debug: impl Fn() -> Option<EchoDebug> + 'static) -> Option<Self> {
            if LocalStorageKeyValueStore::new()
                .get("roostSmoke")
                .as_deref()
                != Some("1")
            {
                return None;
            }
            let window = web_sys::window()?;
            let read = Closure::<dyn Fn() -> JsValue>::new(move || {
                read_debug()
                    .and_then(|debug| JSON::parse(&predict_debug_json(&debug).to_string()).ok())
                    .unwrap_or(JsValue::NULL)
            });
            if Reflect::set(&window, &JsValue::from_str(GLOBAL), read.as_ref()).is_err() {
                tracing::warn!(target: "smoke", "__roostPredictDebug could not be installed");
                return None;
            }
            Some(Self { read })
        }
    }

    impl Drop for PredictDebugHook {
        fn drop(&mut self) {
            let Some(window) = web_sys::window() else {
                return;
            };
            let key = JsValue::from_str(GLOBAL);
            let installed = Reflect::get(&window, &key).unwrap_or(JsValue::UNDEFINED);
            if Object::is(&installed, self.read.as_ref()) {
                let _ = Reflect::delete_property(&window, &key);
            }
        }
    }
}
