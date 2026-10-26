//! The two client-state handlers: the account-scoped list and the all-or-nothing
//! batch set. Every entry is checked before the database is touched.

use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_proto as proto;
use roost_protocol::client_state::{
    MAX_BATCH_ENTRIES, MAX_KEY_BYTES, MAX_VALUE_BYTES, is_synced_key, validate_batch_len,
    validate_key, validate_value,
};

use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::db::CoordDb;
use crate::rpc::service::ok_response;

use super::ClientStateError;
use super::store::{self, ClientStateRow};

/// Every allowlisted key the caller's account holds.
pub async fn handle_client_state_list(
    core: &CoordCore,
    caller: &Caller,
    _request: proto::ClientStateListRequest,
) -> ServiceResult<proto::ClientStateListResponse> {
    let account_id = account_scope(core, caller)?;
    let rows = store::list_rows(&core.services.db, &account_id)
        .await
        .map_err(|error| refuse("list", ClientStateError::from(error)))?;
    ok_response(proto::ClientStateListResponse {
        entries: rows
            .iter()
            .filter(|row| is_synced_key(&row.key))
            .map(row_to_proto)
            .collect(),
        ..Default::default()
    })
}

/// Applies a batch all-or-nothing and returns the stored entry for every requested key.
pub async fn handle_client_state_set(
    core: &CoordCore,
    caller: &Caller,
    request: proto::ClientStateSetRequest,
) -> ServiceResult<proto::ClientStateSetResponse> {
    let account_id = account_scope(core, caller)?;
    let requested = request.entries.len();
    let rows = apply_set(&core.services.db, &account_id, request.entries)
        .await
        .map_err(|error| refuse("set", error))?;
    tracing::debug!(
        target: "client_state",
        operation = "set",
        entries = requested,
        outcome = "applied",
        "client state batch applied"
    );
    ok_response(proto::ClientStateSetResponse {
        entries: rows.iter().map(row_to_proto).collect(),
        ..Default::default()
    })
}

/// The account a browser's state belongs to. A legacy self-hosted browser has no
/// account of its own, so it uses the self-hosted tenant's.
fn account_scope(core: &CoordCore, caller: &Caller) -> Result<String, ConnectError> {
    require_account_device(caller)?;
    match caller.account_id() {
        Some(account_id) => Ok(account_id.to_owned()),
        None => Ok(core.services.boot.require_tenant()?.account_id.clone()),
    }
}

async fn apply_set(
    database: &CoordDb,
    account_id: &str,
    entries: Vec<proto::ClientStateEntry>,
) -> Result<Vec<ClientStateRow>, ClientStateError> {
    validate_batch_len("entries", entries.len()).map_err(|_| ClientStateError::BatchTooLarge {
        limit: MAX_BATCH_ENTRIES,
    })?;
    let batch = entries
        .into_iter()
        .map(validated_row)
        .collect::<Result<Vec<_>, _>>()?;
    store::apply_batch(database, account_id, &batch).await
}

fn validated_row(entry: proto::ClientStateEntry) -> Result<ClientStateRow, ClientStateError> {
    if !is_synced_key(&entry.key) {
        return Err(ClientStateError::NotAllowlisted);
    }
    // Past the allowlist check, `validate_key` can only fail on length.
    validate_key("entries.key", &entry.key).map_err(|_| ClientStateError::KeyTooLong {
        limit: MAX_KEY_BYTES,
    })?;
    validate_value("entries.value", &entry.value).map_err(|_| ClientStateError::ValueTooLong {
        limit: MAX_VALUE_BYTES,
    })?;
    Ok(ClientStateRow {
        key: entry.key,
        value: entry.value,
        updated_ms: entry.updated_ms,
    })
}

fn row_to_proto(row: &ClientStateRow) -> proto::ClientStateEntry {
    proto::ClientStateEntry {
        key: row.key.clone(),
        value: row.value.clone(),
        updated_ms: row.updated_ms,
        ..Default::default()
    }
}

/// Logs one event per refused or failed call, then maps it to a Connect code.
/// Values are never logged: the reason label and the operation are the whole event.
fn refuse(operation: &'static str, error: ClientStateError) -> ConnectError {
    let code = match &error {
        ClientStateError::KeyCapReached { .. } => ErrorCode::ResourceExhausted,
        ClientStateError::Database(_) => ErrorCode::Internal,
        ClientStateError::KeyTooLong { .. }
        | ClientStateError::ValueTooLong { .. }
        | ClientStateError::NotAllowlisted
        | ClientStateError::BatchTooLarge { .. } => ErrorCode::InvalidArgument,
    };
    match &error {
        ClientStateError::Database(_) => tracing::error!(
            target: "client_state",
            operation = operation,
            reason = error.reason(),
            "client state call failed"
        ),
        _ => tracing::warn!(
            target: "client_state",
            operation = operation,
            reason = error.reason(),
            outcome = "rejected",
            "client state call refused"
        ),
    }
    ConnectError::new(code, error.to_string())
}
