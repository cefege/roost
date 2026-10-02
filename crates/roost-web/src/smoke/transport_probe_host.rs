//! `probeTerminalTransport(sessionId)`: one content-free worker control probe on
//! the session's current route, then the route telemetry that probe produced.
//!
//! Answered by `smoke::dispatch`. A peer route is probed through its heartbeat
//! (`Pump::probe_peer_route`); a loopback or Sync route through the core
//! (`ClientEvent::TransportProbeRequested`). Ports
//! `apps/web/src/smoke/smokeTerminalStreamProbe.ts:45-79`.

use roost_client_core::store::sync_feeds::TRANSPORT_PROBE_TIMEOUT_MS;
use roost_client_core::{ClientEvent, Clock as _};
use serde_json::{Value, json};

use super::backdoor::SmokeBackdoor;
use super::paint_wait::sleep_ms;
use super::stream_diagnostics::{DiagnosticClocks, terminal_stream_diagnostics};
use crate::platform::BrowserClock;
use crate::platform::terminal_view_id::mint_probe_request_id;

/// How often the route is re-read while the probe is in flight.
const PROBE_POLL_MS: i32 = 20;

impl SmokeBackdoor {
    /// `probeTerminalTransport(sessionId)`.
    pub(super) async fn probe_terminal_transport_call(
        &self,
        session_id: &str,
    ) -> Result<Value, String> {
        let clock = BrowserClock::new();
        let started_ms = clock.now_ms();
        let before = self.route_snapshot(session_id);
        let kind = before["active"]["kind"]
            .as_str()
            .ok_or("terminal session has no route to probe")?
            .to_owned();
        if kind == "webrtc" {
            let token = self
                .elected_token(session_id)
                .ok_or("terminal peer route has no carrier")?;
            if !self.pump.probe_peer_route(&token, started_ms) {
                return Err("terminal peer route has no carrier".to_owned());
            }
        } else {
            let request_id =
                mint_probe_request_id().ok_or("no crypto.randomUUID: cannot mint a probe id")?;
            self.pump.dispatch(ClientEvent::TransportProbeRequested {
                session_id: session_id.to_owned(),
                request_id,
            });
        }
        loop {
            let route = self.route_snapshot(session_id);
            let active = &route["active"];
            if active["kind"] != before["active"]["kind"]
                || active["worker_epoch"] != before["active"]["worker_epoch"]
                || active["peer_id"] != before["active"]["peer_id"]
            {
                return Err(format!(
                    "terminal route changed during the {kind} control probe"
                ));
            }
            let elapsed_ms = clock.now_ms().saturating_sub(started_ms);
            // A sample answered before this call started is an older probe's.
            let answered_since = active["probe_age_ms"]
                .as_u64()
                .is_some_and(|age_ms| age_ms <= elapsed_ms);
            if answered_since && !active["worker_control_rtt_ms"].is_null() {
                return Ok(json!({
                    "transport_kind": active["kind"],
                    "worker_epoch": active["worker_epoch"],
                    "candidate_type": active["candidate_type"],
                    "worker_control_rtt_ms": active["worker_control_rtt_ms"],
                    "pending_input_count": route["pending_input_count"],
                }));
            }
            if elapsed_ms >= TRANSPORT_PROBE_TIMEOUT_MS {
                return Err(
                    "terminal worker control probe did not produce route telemetry".to_owned(),
                );
            }
            sleep_ms(PROBE_POLL_MS).await;
        }
    }

    fn route_snapshot(&self, session_id: &str) -> Value {
        let clocks = DiagnosticClocks {
            monotonic_ms: BrowserClock::new().now_ms(),
            epoch_ms: js_sys::Date::now(),
        };
        let core = self.pump.core();
        let core = core.borrow();
        terminal_stream_diagnostics(core.store(), session_id, clocks)
            .remove("route")
            .unwrap_or(Value::Null)
    }

    fn elected_token(&self, session_id: &str) -> Option<roost_client_core::TerminalToken> {
        let core = self.pump.core();
        let core = core.borrow();
        core.store()
            .routes
            .route(session_id)
            .map(|route| route.token.clone())
    }
}
