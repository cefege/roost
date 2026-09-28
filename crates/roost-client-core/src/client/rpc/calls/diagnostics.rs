//! Asking the coordinator for its layered diagnostic snapshot, with the
//! browser's own layer attached as the opaque SPA payload.
//!
//! Called by roost-web's smoke backdoor (`terminalStreamProbe`) through
//! `CoordRpc::call`. v2 call site: `apps/web/src/smoke/smokeTerminalStreamProbe.ts:30-32`
//! (`coordClient.diagSnapshot`).

use roost_proto::{DiagSnapshotRequest, DiagSnapshotResponse};

use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// `DiagSnapshot`: the coordinator's and routed workers' state as JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagSnapshot {
    /// The browser layer, serialized; the coordinator embeds it verbatim.
    pub spa_state_json: String,
}

impl UnaryMethod for DiagSnapshot {
    const METHOD: &'static str = "DiagSnapshot";
    /// The snapshot JSON, undecoded: its shape is the coordinator's.
    type Response = String;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &DiagSnapshotRequest {
                spa_state_json: self.spa_state_json.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<String, RpcCodecError> {
        let response: DiagSnapshotResponse = decode_message(Self::METHOD, body)?;
        Ok(response.snapshot_json)
    }
}
