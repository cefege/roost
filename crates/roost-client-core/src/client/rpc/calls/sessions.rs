//! Session mutations a surface asks for directly: spawn, kill, rename.
//!
//! Called by the smoke backdoor (`spawnShell`, `kill`, `cleanupCreated`) and
//! the session surfaces through `CoordRpc::call`. The field mapping is v2's
//! call sites: `apps/web/src/smoke/smokeCreatedResources.ts:84-99`,
//! `apps/web/src/lib/spawnSession.ts`, `apps/web/src/lib/closeSession.ts`.

use roost_proto::{
    SessionsKillRequest, SessionsKillResponse, SessionsRenameRequest, SessionsRenameResponse,
    SessionsSpawnRequest, SessionsSpawnResponse,
};

use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// `SessionsSpawn`: open a PTY on one worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnSession {
    /// The worker that owns the PTY.
    pub worker_fp: String,
    /// `shell`, or an agent launcher kind.
    pub kind: String,
    /// The folder the PTY starts in.
    pub folder: String,
    /// The pane's columns, when a pane is waiting for this session.
    pub cols: Option<u32>,
    /// The pane's rows.
    pub rows: Option<u32>,
    /// A caller-minted id for an optimistic spawn; the worker reuses it.
    pub session_id: Option<String>,
}

/// What `SessionsSpawn` answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnedSession {
    /// The session's id.
    pub session_id: String,
    /// The PTY channel on the worker.
    pub channel_id: u32,
}

impl UnaryMethod for SpawnSession {
    const METHOD: &'static str = "SessionsSpawn";
    type Response = SpawnedSession;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &SessionsSpawnRequest {
                worker_fp: self.worker_fp.clone(),
                kind: self.kind.clone(),
                folder: self.folder.clone(),
                cols: self.cols,
                rows: self.rows,
                session_id: self.session_id.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<SpawnedSession, RpcCodecError> {
        let response: SessionsSpawnResponse = decode_message(Self::METHOD, body)?;
        Ok(SpawnedSession {
            session_id: response.session_id,
            channel_id: response.channel_id,
        })
    }
}

/// `SessionsKill`: end a session's PTY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KillSession {
    /// The session.
    pub session_id: String,
    /// Skip the graceful hangup.
    pub force: bool,
}

impl UnaryMethod for KillSession {
    const METHOD: &'static str = "SessionsKill";
    /// Whether the coordinator accepted the kill.
    type Response = bool;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &SessionsKillRequest {
                session_id: self.session_id.clone(),
                force: self.force,
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<bool, RpcCodecError> {
        let response: SessionsKillResponse = decode_message(Self::METHOD, body)?;
        Ok(response.accepted)
    }
}

/// `SessionsRename`: set or clear a session's custom title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameSession {
    /// The session.
    pub session_id: String,
    /// The new title; empty clears it.
    pub title: String,
}

impl UnaryMethod for RenameSession {
    const METHOD: &'static str = "SessionsRename";
    /// Whether the rename was applied.
    type Response = bool;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &SessionsRenameRequest {
                session_id: self.session_id.clone(),
                title: self.title.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<bool, RpcCodecError> {
        let response: SessionsRenameResponse = decode_message(Self::METHOD, body)?;
        Ok(response.ok)
    }
}
