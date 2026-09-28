//! The wire name of a call, and which calls refuse to answer without a device.
//!
//! `RpcCall` is the state machine's vocabulary and deliberately carries no
//! string, so this is the one place that vocabulary is rendered to the wire. A
//! method name living in two files is how a call is sent to one method and
//! classified as another.
//!
//! Ported from `apps/web/src/client/rpc/connect.ts:24-34,95-97`. The names are
//! the ones `protocol/proto/roost/v1/coordinator.proto:936-1025` declares, which
//! is the only source of truth for them.

use crate::effect::RpcCall;

/// The service prefix the coordinator publishes. The transport prepends it, so a
/// caller that spells a whole path out by hand is a second place the service
/// name lives.
pub const COORDINATOR_SERVICE_PATH_PREFIX: &str = "/roost.v1.CoordinatorService/";

/// The methods whose `Unauthenticated` is a DEVICE rejection rather than a
/// retryable one.
///
/// Listed by name rather than derived from the request, because the request
/// carries no idea what it is for: the coordinator decided this list, and it is
/// the coordinator's list to keep. A method absent here answers `Unauthenticated`
/// for reasons a retry fixes — an expired credential, a clock skew — so treating
/// it as a device rejection would show the pairing page to a user whose pairing
/// is fine.
///
/// v2 keyed this on the full RPC path (`connect.ts:24-34`); the same nine
/// methods, keyed on the method name, because the path is this file's rendering
/// of the name and not an independent input.
const DEVICE_AUTH_REQUIRED_METHODS: [&str; 9] = [
    "WorkersList",
    "SessionsList",
    "WorkspacesList",
    "TasksList",
    "McpList",
    "DevicesList",
    "DevicesRevoke",
    "DevicesRotateCurrent",
    "PairApprovalStatus",
];

/// The Connect method a call goes out as.
pub fn connect_method(call: &RpcCall) -> &'static str {
    match call {
        RpcCall::CoordIdentity { .. } => "AuthCoordIdentity",
        RpcCall::SessionsList { .. } => "SessionsList",
        RpcCall::WorkersList { .. } => "WorkersList",
        RpcCall::WorkspacesList { .. } => "WorkspacesList",
        RpcCall::TasksList { .. } => "TasksList",
        RpcCall::McpList { .. } => "McpList",
        RpcCall::PairList { .. } => "PairList",
        RpcCall::RedeemPairToken { .. } => "AuthRedeemBrowser",
        RpcCall::FilesListDir { .. } => "FilesListDir",
        RpcCall::FilesMkdir { .. } => "FilesMkdir",
        RpcCall::SessionsSearchGlobal { .. } => "SessionsSearchGlobal",
        RpcCall::SessionsCancelGlobalSearch { .. } => "SessionsCancelGlobalSearch",
    }
}

/// The call id a call is correlated by, for the request and its answer.
pub fn connect_call_id(call: &RpcCall) -> u64 {
    match call {
        RpcCall::CoordIdentity { call_id }
        | RpcCall::SessionsList { call_id, .. }
        | RpcCall::WorkersList { call_id }
        | RpcCall::WorkspacesList { call_id }
        | RpcCall::TasksList { call_id }
        | RpcCall::McpList { call_id }
        | RpcCall::PairList { call_id }
        | RpcCall::RedeemPairToken { call_id, .. }
        | RpcCall::FilesListDir { call_id, .. }
        | RpcCall::FilesMkdir { call_id, .. }
        | RpcCall::SessionsSearchGlobal { call_id, .. }
        | RpcCall::SessionsCancelGlobalSearch { call_id, .. } => *call_id,
    }
}

/// The full RPC path, for a log line and for a host that builds a URL itself.
pub fn rpc_path(method: &str) -> String {
    format!("{COORDINATOR_SERVICE_PATH_PREFIX}{method}")
}

/// Whether this method refuses to answer an unauthenticated caller with anything
/// but a device rejection.
pub fn requires_device_auth(method: &str) -> bool {
    DEVICE_AUTH_REQUIRED_METHODS.contains(&method)
}
