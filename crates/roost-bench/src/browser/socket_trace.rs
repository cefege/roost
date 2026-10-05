//! A tab's WebSocket frames, as CDP saw them leave and arrive, stamped on the
//! page's epoch clock (the one `window.__bench` arms and hits on). Called by
//! the echo scenario to split a keystroke into its server and browser halves;
//! depends on chromiumoxide's `Network` events.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use chromiumoxide::cdp::browser_protocol::network::{
    EnableParams, EventRequestWillBeSent, EventWebSocketFrameReceived, EventWebSocketFrameSent,
};
use futures::StreamExt as _;
use serde_json::Value;
use tokio::task::JoinHandle;

use crate::browser::BenchPage;
use crate::error::BenchError;

const CALIBRATION_DEADLINE: Duration = Duration::from_secs(2);
const CALIBRATION_MARK: &str = "cal=";

/// Which way a frame crossed the socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceKind {
    Sent,
    Received,
}

/// One WebSocket message. `bytes` is the CDP payload length (base64 for a
/// binary frame), a label rather than a wire size.
#[derive(Debug, Clone)]
pub struct TraceEvent {
    pub kind: TraceKind,
    pub epoch_ms: f64,
    pub bytes: usize,
}

/// Frames collected since the last `drain`, until dropped.
#[derive(Debug)]
pub struct SocketTrace {
    events: Arc<Mutex<Vec<TraceEvent>>>,
    pumps: Vec<JoinHandle<()>>,
}

impl BenchPage {
    /// Start recording every WebSocket frame of this tab. CDP stamps frames on
    /// a monotonic clock; one throwaway fetch, whose request event carries
    /// both that clock and wall time, maps them onto the page's epoch clock.
    pub async fn start_socket_trace(&self) -> Result<SocketTrace, BenchError> {
        self.page
            .execute(EnableParams::default())
            .await
            .map_err(BenchError::browser)?;
        let mono_to_epoch_ms = self.calibrate_monotonic_clock().await?;
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut sent = self
            .page
            .event_listener::<EventWebSocketFrameSent>()
            .await
            .map_err(BenchError::browser)?;
        let mut received = self
            .page
            .event_listener::<EventWebSocketFrameReceived>()
            .await
            .map_err(BenchError::browser)?;
        let sent_sink = Arc::clone(&events);
        let sent_pump = tokio::spawn(async move {
            while let Some(frame) = sent.next().await {
                record_frame(
                    &sent_sink,
                    TraceKind::Sent,
                    *frame.timestamp.inner() * 1000.0 + mono_to_epoch_ms,
                    frame.response.payload_data.len(),
                );
            }
        });
        let received_sink = Arc::clone(&events);
        let received_pump = tokio::spawn(async move {
            while let Some(frame) = received.next().await {
                record_frame(
                    &received_sink,
                    TraceKind::Received,
                    *frame.timestamp.inner() * 1000.0 + mono_to_epoch_ms,
                    frame.response.payload_data.len(),
                );
            }
        });
        tracing::info!(mono_to_epoch_ms, "socket trace started");
        Ok(SocketTrace {
            events,
            pumps: vec![sent_pump, received_pump],
        })
    }

    /// `wallTime − timestamp` of a request this tab is made to send.
    async fn calibrate_monotonic_clock(&self) -> Result<f64, BenchError> {
        let mut requests = self
            .page
            .event_listener::<EventRequestWillBeSent>()
            .await
            .map_err(BenchError::browser)?;
        self.eval::<Value>(
            "fetch('/icon.svg?cal=' + Math.random(), { cache: 'no-store' }).then(() => true, () => true)",
        )
        .await?;
        let calibration = tokio::time::timeout(CALIBRATION_DEADLINE, async {
            while let Some(request) = requests.next().await {
                if request.request.url.contains(CALIBRATION_MARK) {
                    return Some(
                        *request.wall_time.inner() * 1000.0 - *request.timestamp.inner() * 1000.0,
                    );
                }
            }
            None
        })
        .await;
        match calibration {
            Ok(Some(offset)) => Ok(offset),
            Ok(None) | Err(_) => Err(BenchError::Browser("no calibration request".to_string())),
        }
    }
}

impl SocketTrace {
    /// Every frame recorded since the previous call, ordered by page time.
    pub fn drain(&self) -> Vec<TraceEvent> {
        let mut events = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        let mut drained = std::mem::take(&mut *events);
        drained.sort_by(|left, right| left.epoch_ms.total_cmp(&right.epoch_ms));
        drained
    }
}

impl Drop for SocketTrace {
    fn drop(&mut self) {
        for pump in &self.pumps {
            pump.abort();
        }
    }
}

fn record_frame(sink: &Mutex<Vec<TraceEvent>>, kind: TraceKind, epoch_ms: f64, bytes: usize) {
    sink.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(TraceEvent {
            kind,
            epoch_ms,
            bytes,
        });
}
