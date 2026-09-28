//! The typed worker results the pending-RPC table settles beside an `rpc-ok`
//! body: input, pipeline sample, route claim, transport probe.
//! Ports the typed `resolvePendingRpc(requestId, frame.value, workerFp)` calls of
//! `apps/coord/src/workers/worker-frame-dispatch.ts:314-336` and the typed
//! `createPendingRpc<T>` waits of `apps/coord/src/workers/worker-send.ts`. Filled by
//! `worker_link::live_frames` and the route-result owner; read by `TerminalWorkerRequest`.

use roost_proto::{
    WTerminalInputRouteResult, WTerminalPipelineSnapshot, WTerminalTransportProbeResult,
};
use roost_protocol::wire::coord_worker::InputResult;

/// One typed result frame, carried to its waiter as the frame decoded it.
///
/// Typed rather than re-encoded as JSON: an input result settles every
/// keystroke batch, and a round trip through `serde_json::Value` would be two
/// allocations per field on that path to arrive back at the same struct.
#[derive(Debug, Clone, PartialEq)]
pub enum TypedWorkerResult {
    /// The keeper-completed outcome of one input or prompt write.
    Input(InputResult),
    /// One bounded terminal-pipeline diagnostic sample.
    PipelineSnapshot(WTerminalPipelineSnapshot),
    /// The outcome of one terminal input-route claim.
    InputRoute(WTerminalInputRouteResult),
    /// The outcome of one terminal transport probe.
    TransportProbe(WTerminalTransportProbeResult),
}

impl TypedWorkerResult {
    /// The coordinator's correlation id the result echoes.
    #[must_use]
    pub fn request_id(&self) -> &str {
        match self {
            Self::Input(result) => &result.request_id,
            Self::PipelineSnapshot(result) => &result.request_id,
            Self::InputRoute(result) => &result.request_id,
            Self::TransportProbe(result) => &result.request_id,
        }
    }

    /// The wire spelling of this result's frame, for a mismatch refusal.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Input(_) => InputResult::KIND,
            Self::PipelineSnapshot(_) => WTerminalPipelineSnapshot::KIND,
            Self::InputRoute(_) => WTerminalInputRouteResult::KIND,
            Self::TransportProbe(_) => WTerminalTransportProbeResult::KIND,
        }
    }
}

/// A payload a pending RPC may be settled with, and how to take it back out.
///
/// The waiter names the type it expects; a result of any other kind under the
/// same correlation id is a worker answering the wrong question, which the
/// waiter refuses rather than misreads.
pub trait TypedResult: Sized + Send + 'static {
    /// The wire spelling of the frame that carries this payload.
    const KIND: &'static str;

    /// This payload, or the kind of the result that arrived instead.
    fn from_typed(result: TypedWorkerResult) -> Result<Self, &'static str>;
}

impl TypedResult for InputResult {
    const KIND: &'static str = "input-result";

    fn from_typed(result: TypedWorkerResult) -> Result<Self, &'static str> {
        match result {
            TypedWorkerResult::Input(result) => Ok(result),
            other => Err(other.kind()),
        }
    }
}

impl TypedResult for WTerminalPipelineSnapshot {
    const KIND: &'static str = "terminal-pipeline-snapshot";

    fn from_typed(result: TypedWorkerResult) -> Result<Self, &'static str> {
        match result {
            TypedWorkerResult::PipelineSnapshot(result) => Ok(result),
            other => Err(other.kind()),
        }
    }
}

impl TypedResult for WTerminalInputRouteResult {
    const KIND: &'static str = "terminal-input-route-result";

    fn from_typed(result: TypedWorkerResult) -> Result<Self, &'static str> {
        match result {
            TypedWorkerResult::InputRoute(result) => Ok(result),
            other => Err(other.kind()),
        }
    }
}

impl TypedResult for WTerminalTransportProbeResult {
    const KIND: &'static str = "terminal-transport-probe-result";

    fn from_typed(result: TypedWorkerResult) -> Result<Self, &'static str> {
        match result {
            TypedWorkerResult::TransportProbe(result) => Ok(result),
            other => Err(other.kind()),
        }
    }
}
