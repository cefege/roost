//! One page of the coordinator's audit log.
//!
//! Called by roost-web's Settings audit pane. v2 call site:
//! `apps/web/src/components/Settings/AuditLogPane.tsx` (`coordClient.auditList`).
//! `next_cursor` is the id of the last row on the page, which is what the
//! coordinator's own `WHERE a.id < ?1` statement pages on.

use roost_proto::{AuditListRequest, AuditListResponse};

use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// One audit row, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AuditLogRow {
    /// The row's monotonic id; also the page cursor.
    pub id: u64,
    /// Wall-clock seconds the call landed.
    pub ts: u64,
    /// The authorized key that made the call, when one did.
    pub caller_fp: Option<String>,
    /// That key's label, when the coordinator knows one.
    pub caller_label: Option<String>,
    /// The Connect method.
    pub method: String,
    /// The request path.
    pub path: String,
    /// The response status.
    pub status: u32,
    /// The request's trace id, when the caller sent one.
    pub trace_id: Option<String>,
}

/// One page, and the cursor the next page starts before.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AuditLogPage {
    /// The rows, newest first.
    pub rows: Vec<AuditLogRow>,
    /// The cursor for the next page; `None` when this was the last one.
    pub next_cursor: Option<String>,
}

/// `AuditList`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ListAuditRows {
    /// Page from strictly before this id; `None` starts at the newest row.
    pub cursor: Option<String>,
    /// How many rows to return; the coordinator clamps this to its own maximum.
    pub limit: Option<u32>,
    /// Restrict to one authorized key.
    pub caller_fp: Option<String>,
    /// Restrict to one Connect method.
    pub method: Option<String>,
}

impl UnaryMethod for ListAuditRows {
    const METHOD: &'static str = "AuditList";
    type Response = AuditLogPage;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &AuditListRequest {
                cursor: self.cursor.clone(),
                limit: self.limit,
                caller_fp: self.caller_fp.clone(),
                method: self.method.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<AuditLogPage, RpcCodecError> {
        let response: AuditListResponse = decode_message(Self::METHOD, body)?;
        Ok(AuditLogPage {
            rows: response
                .rows
                .iter()
                .map(|row| AuditLogRow {
                    id: row.id,
                    ts: row.ts,
                    caller_fp: row.caller_fp.clone(),
                    caller_label: row.caller_label.clone(),
                    method: row.method.clone(),
                    path: row.path.clone(),
                    status: row.status,
                    trace_id: row.trace_id.clone(),
                })
                .collect(),
            next_cursor: response.next_cursor,
        })
    }
}
