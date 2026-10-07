//! The collaborators a session test drives, each one recording what it was
//! asked. `Harness` in the parent module wires them together; the four test
//! binaries that include this module each reach for a different subset, which
//! is why both allows below are stated in terms of binaries rather than items.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)]
// `#[allow(dead_code)]` because the four binaries that include this file call
// DIFFERENT subsets of it: `closed_events` is `session_lifecycle`'s alone,
// while `with_survivor`, `delivered` and `killed` are `session_adoption`'s
// alone, and `session_binding` and `session_resize` call neither. Those four
// names are the entire lint list, read from a run of all four binaries with
// this allow deleted — so it is measured, not assumed. Deleting or narrowing
// one to quiet a warning in one binary would break a caller in another.

use std::sync::{Arc, Mutex};

use roost_observability::clock::EventClock;
use roost_protocol::wire::brand::ChannelId;
use roost_protocol::wire::event::SessionEvent;
use roost_worker::event_store::{DurableEventKind, Reservation, Store};
use roost_worker::session::binding::{CellDelivery, ChannelDelivery};
use roost_worker::session::sinks::{
    ChannelBinding, EventFuture, SessionEventError, SessionEventSink,
};
use roost_worker::session::spawn::{ShellSpawner, ShellSpecResolver};
use roost_worker::session::types::SessionRecord;
use roost_worker::shell_spec::ShellSpec;

use super::NOW;

/// A clock that does not move, so a timestamp in an assertion is the one the
/// test wrote.
#[derive(Debug)]
pub struct PinnedClock;

impl EventClock for PinnedClock {
    fn now_epoch_ms(&self) -> i64 {
        NOW
    }
    fn mono_ns(&self) -> u64 {
        5_000_000_000
    }
}

/// The durable boundary, over a real store, recording what it published.
#[derive(Default)]
pub struct RecordingSink {
    pub store: Mutex<Store>,
    pub emitted: Mutex<Vec<SessionEvent>>,
    pub fail_next: Mutex<bool>,
}

impl SessionEventSink for RecordingSink {
    fn reserve(
        &self,
        kind: DurableEventKind,
    ) -> EventFuture<'_, Result<Reservation, SessionEventError>> {
        // ASYNC IN SHAPE, SYNCHRONOUS IN FACT. The production sink's futures do
        // a database round trip; this one's work is already done by the time it
        // is built, so the future has nothing to await. Returning it as a future
        // rather than a plain value is what keeps this fake honest about the
        // seam — a fake whose signature quietly diverged is how a caller keeps
        // compiling against a shape the product no longer has.
        let reserved = self
            .store
            .lock()
            .expect("held")
            .reserve_default(kind)
            .map_err(SessionEventError::Reserve);
        Box::pin(std::future::ready(reserved))
    }

    fn hold(&self, reservation: Reservation) -> EventFuture<'_, ()> {
        self.store
            .lock()
            .expect("held")
            .hold(reservation)
            .expect("a fresh claim is live");
        Box::pin(std::future::ready(()))
    }

    fn release(&self, reservation: Reservation) -> EventFuture<'_, ()> {
        let _ = self.store.lock().expect("held").release(reservation);
        Box::pin(std::future::ready(()))
    }

    fn emit<'a>(
        &'a self,
        event: &'a SessionEvent,
        reservation: Option<Reservation>,
    ) -> EventFuture<'a, Result<(), SessionEventError>> {
        let published = (|| {
            if *self.fail_next.lock().expect("held") {
                return Err(SessionEventError::Unclassifiable(
                    "the store is refusing writes".to_string(),
                ));
            }
            if let Some(reservation) = reservation {
                let bytes = serde_json::to_vec(event)
                    .expect("an event serialises")
                    .len();
                let kind = event_kind(event);
                self.store
                    .lock()
                    .expect("held")
                    .append(reservation, kind, bytes)
                    .map_err(SessionEventError::Append)?;
            }
            self.emitted.lock().expect("held").push(event.clone());
            Ok(())
        })();
        Box::pin(std::future::ready(published))
    }
}

impl RecordingSink {
    pub fn published(&self) -> Vec<SessionEvent> {
        self.emitted.lock().expect("held").clone()
    }
    pub fn closed_events(&self) -> usize {
        self.published()
            .iter()
            .filter(|event| matches!(event, SessionEvent::Closed { .. }))
            .count()
    }
}

fn event_kind(event: &SessionEvent) -> DurableEventKind {
    match event {
        SessionEvent::Opened { .. } => DurableEventKind::Opened,
        SessionEvent::Closed { .. } => DurableEventKind::Closed,
        _ => DurableEventKind::State,
    }
}

/// The delivery, answering with what it was handed and nothing else.
#[derive(Default)]
pub struct RecordingDelivery {
    pub parsed: Mutex<Vec<Vec<u8>>>,

    pub frozen: Mutex<Vec<ChannelId>>,
    pub capture: Mutex<Vec<u8>>,
}

impl ChannelDelivery for RecordingDelivery {
    fn ingest_output(&self, _record: &mut SessionRecord, chunk: &[u8], _now_ms: i64) {
        self.parsed.lock().expect("held").push(chunk.to_vec());
    }
    fn freeze_capture(&self, channel_id: ChannelId, _now_ms: i64) -> bool {
        self.frozen.lock().expect("held").push(channel_id);
        true
    }
    fn close_capture(
        &self,
        _channel_id: ChannelId,
    ) -> roost_worker::session::binding::CapturedOutput {
        roost_worker::session::binding::CapturedOutput {
            bytes: std::mem::take(&mut *self.capture.lock().expect("held")),
            overflowed: false,
        }
    }
}

/// The emitter's channel registration, counted.
#[derive(Default)]
pub struct CountingCells {
    pub installed: Mutex<Vec<(u16, String)>>,
    pub forgotten: Mutex<Vec<u16>>,
    /// `note_input_echo`, `cancel_cell_emission` and `release_sync_output_hold`
    /// calls, by name and channel, in order.
    pub emission_calls: Mutex<Vec<(&'static str, u16)>>,
}

impl CountingCells {
    fn note(&self, call: &'static str, channel_id: ChannelId) {
        self.emission_calls
            .lock()
            .expect("held")
            .push((call, channel_id.as_u32() as u16));
    }
}

impl CellDelivery for CountingCells {
    fn install_stream(&mut self, channel_id: ChannelId, stream_id: &str) {
        self.installed
            .lock()
            .expect("held")
            .push((channel_id.as_u32() as u16, stream_id.to_string()));
    }
    fn forget_channel(&mut self, channel_id: ChannelId) {
        self.forgotten
            .lock()
            .expect("held")
            .push(channel_id.as_u32() as u16);
    }
    fn note_input_echo(&mut self, channel_id: ChannelId, _now_ms: i64) {
        self.note("note_input_echo", channel_id);
    }
    fn cancel_cell_emission(&mut self, channel_id: ChannelId) {
        self.note("cancel_cell_emission", channel_id);
    }
    fn release_sync_output_hold(&mut self, channel_id: ChannelId) {
        self.note("release_sync_output_hold", channel_id);
    }
    fn channel_diagnostics(
        &self,
        _channel_id: ChannelId,
    ) -> roost_worker::session::diagnostics::ChannelDiagnostics {
        roost_worker::session::diagnostics::ChannelDiagnostics::default()
    }
}

/// A spawner that never runs: these tests are about the paths around a spawn.
pub struct NeverSpawns;

impl ShellSpawner for NeverSpawns {
    fn spawn_channel(
        &self,
        _channel_id: ChannelId,
        _spec: &ShellSpec,
        _cols: u16,
        _rows: u16,
        _binding: Arc<dyn ChannelBinding>,
    ) -> Result<u32, String> {
        Err("this harness does not open a pty".to_string())
    }
    fn kill_channel(&self, _channel_id: ChannelId) {}
}

pub struct FixedResolver {
    pub spec: ShellSpec,
}

impl ShellSpecResolver for FixedResolver {
    fn resolve_shell_spec(&self, _cwd: &str, _session_id: &str) -> Result<ShellSpec, String> {
        Ok(self.spec.clone())
    }
}

/// The harness's recorder behind the locked trait object a `RecordBinding` is
/// built from.
///
/// The harness hands its recorder out as the CONCRETE `Arc<RecordingDelivery>`,
/// because that is what the assertions read, while `SessionManager` and a
/// hand-built `RecordBinding` both want `Arc<Mutex<dyn ChannelDelivery>>`. An
/// `as` cast cannot bridge those two — it is E0605, a non-primitive cast — so
/// the forwarding wrapper is the only way one recorder answers to both types.
/// It lives here rather than in a test file so that there is ONE of them: a
/// second copy would be a second answer to "what does a hand-built binding
/// parse into", which is how a binding ends up writing to a recorder no
/// assertion reads.
struct SharedDelivery(Arc<RecordingDelivery>);

impl ChannelDelivery for SharedDelivery {
    fn ingest_output(&self, record: &mut SessionRecord, chunk: &[u8], now_ms: i64) {
        self.0.ingest_output(record, chunk, now_ms);
    }

    fn freeze_capture(&self, channel_id: ChannelId, now_ms: i64) -> bool {
        self.0.freeze_capture(channel_id, now_ms)
    }

    fn close_capture(
        &self,
        channel_id: ChannelId,
    ) -> roost_worker::session::binding::CapturedOutput {
        self.0.close_capture(channel_id)
    }
}

/// The delivery a hand-built binding is fed through, wired to the SAME recorder
/// the harness's assertions read — which is the whole point of the shim.
pub fn shared_delivery(recorder: &Arc<RecordingDelivery>) -> Arc<Mutex<dyn ChannelDelivery>> {
    Arc::new(Mutex::new(SharedDelivery(Arc::clone(recorder))))
}
