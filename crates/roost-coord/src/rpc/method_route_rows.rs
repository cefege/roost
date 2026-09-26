//! The per-domain method tables behind `METHOD_ROUTES`, split by v2's own
//! `apps/coord/src/<domain>/` folders so each table reads on its own.
//!
//! The tables are concatenated in the proto's declaration order, which is what
//! `tests/method_route_coverage.rs` asserts -- so the split costs navigability
//! nothing and the ordering guarantee survives it.
//!
//! Kept as plain data rather than generated: 103 rows of cited policy is exactly
//! what a build-time macro would hide.
//!
//! `#[rustfmt::skip]` on every table, deliberately. This is DATA, and rustfmt
//! reformats one `route(...)` call to six lines, which turns a readable 200-line
//! table into 700 lines of ceremony and pushes the file over the size cap for no
//! gain. The one thing formatting buys here -- aligned columns a reader can scan
//! -- is the one thing this file most needs, and `#[rustfmt::skip]` is how a
//! data table keeps it.

use super::method_route::{AuthRequirement, MethodRoute, PortStatus};

const fn route(
    method: &'static str,
    domain: &'static str,
    auth: AuthRequirement,
    status: PortStatus,
) -> MethodRoute {
    MethodRoute {
        method,
        domain,
        auth,
        status,
    }
}

/// the worker registry: registration, heartbeat, rename, delete, deploy.
#[rustfmt::skip]
pub const ROWS_WORKERS: &[MethodRoute] = &[
    route("WorkersList", "workers", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("WorkersRegister", "workers", AuthRequirement::Worker, PortStatus::AwaitingDomainPort),
    route("WorkersHeartbeat", "workers", AuthRequirement::Worker, PortStatus::AwaitingDomainPort),
    route("WorkersRename", "workers", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("WorkersDelete", "workers", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("WorkersDeployStart", "workers", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("WorkersDeployOutput", "workers", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
];

/// keeper-update preparation, the host-local change that replaces the binary every live PTY depends on.
#[rustfmt::skip]
pub const ROWS_DEPLOY: &[MethodRoute] = &[route(
    "WorkersPrepareKeeperUpdate",
    "deploy",
    AuthRequirement::DeviceOnHost,
    PortStatus::AwaitingDomainPort,
)];

/// the session lifecycle, scrollback, and direct-terminal grants.
#[rustfmt::skip]
pub const ROWS_SESSIONS: &[MethodRoute] = &[
    route("SessionsList", "sessions", AuthRequirement::DeviceOrOwnWorkerRecovery, PortStatus::AwaitingDomainPort),
    route("SessionsSpawn", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("SessionsAttach", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("SessionsKill", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("SessionsRename", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("SessionsInput", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("SessionsCursorPos", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("SessionsAssignWorkspace", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("SessionsGetScrollbackCells", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("SessionsSearchScrollback", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("SessionsCancelScrollbackSearch", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("SessionsGrantLocalTerminal", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("SessionsNegotiateLocalTerminalPeer", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("WorkspacesList", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("WorkspacesCreate", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("WorkspacesUpdate", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("WorkspacesDelete", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("WorkspacesSetSessions", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("TasksList", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("TasksEnqueue", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("TasksNextPending", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("TasksSetState", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("TasksCancel", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("McpList", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("McpCreate", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("McpDelete", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("McpPublish", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
];

/// agent status, the fenced agent prompt, and agent configuration.
#[rustfmt::skip]
pub const ROWS_AGENTS: &[MethodRoute] = &[
    route("SessionsPrompt", "agents", AuthRequirement::DevicePlusFence, PortStatus::AwaitingDomainPort),
    route("AgentStatusGet", "agents", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("AgentStatusList", "agents", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("AgentStatusWait", "agents", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("AgentConfigGet", "agents", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("AgentConfigSet", "agents", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
];

/// global session search across every worker.
#[rustfmt::skip]
pub const ROWS_SEARCH: &[MethodRoute] = &[
    route("SessionsSearchGlobal", "search", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("SessionsCancelGlobalSearch", "search", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
];

/// files, the attachment chunk relay, direct grants, and peer negotiation.
#[rustfmt::skip]
pub const ROWS_ATTACHMENTS: &[MethodRoute] = &[
    route("SessionsNegotiateAttachmentPeer", "attachments", AuthRequirement::DevicePlusFence, PortStatus::AwaitingDomainPort),
    route("FilesRead", "attachments", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("FilesReadChunk", "attachments", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("FilesListDir", "attachments", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("FilesMkdir", "attachments", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("AttachmentsGrantDirect", "attachments", AuthRequirement::DevicePlusFence, PortStatus::AwaitingDomainPort),
    route("AttachmentsDirectStatus", "attachments", AuthRequirement::DevicePlusFence, PortStatus::AwaitingDomainPort),
    route("AttachFileChunk", "attachments", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("AttachmentProbe", "attachments", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("ListAttachments", "attachments", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("DeleteAttachment", "attachments", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
];

/// auth, pairing and device lifecycle.
#[rustfmt::skip]
pub const ROWS_AUTH: &[MethodRoute] = &[
    route("AuthCoordIdentity", "auth", AuthRequirement::Public, PortStatus::AwaitingDomainPort),
    route("AuthDashboardAccess", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("AuthLogout", "auth", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("AuthOwnerActivate", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("AuthPasswordResetRequest", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("AuthPasswordResetRedeem", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("AuthPasswordLogin", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("AuthFederatedContinue", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("AuthCredentialsGet", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("AuthPasswordAdd", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("AuthFederatedLinkBegin", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("AuthFederatedLink", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("AuthMintBootstrap", "auth", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("AuthRedeemWorker", "auth", AuthRequirement::Public, PortStatus::AwaitingDomainPort),
    route("AuthRedeemBrowser", "auth", AuthRequirement::Public, PortStatus::AwaitingDomainPort),
    route("AuthMintCoordinatorRelocation", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("AuthRedeemCoordinatorRelocation", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("DevicesList", "auth", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("DevicesRevoke", "auth", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("DevicesRotateCurrent", "auth", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("CoordinatorMovePreflight", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("CoordinatorMoveStart", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("CoordinatorMoveStatus", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("PairCreate", "auth", AuthRequirement::Public, PortStatus::AwaitingDomainPort),
    route("PairPoll", "auth", AuthRequirement::Public, PortStatus::AwaitingDomainPort),
    route("PairList", "auth", AuthRequirement::DeviceOrOwnWorkerRecovery, PortStatus::AwaitingDomainPort),
    route("PairApprove", "auth", AuthRequirement::DeviceOrOwnWorkerRecovery, PortStatus::AwaitingDomainPort),
    route("PairConfirm", "auth", AuthRequirement::Public, PortStatus::AwaitingDomainPort),
    route("PairDeny", "auth", AuthRequirement::DeviceOrOwnWorkerRecovery, PortStatus::AwaitingDomainPort),
    route("PairApprovalStatus", "auth", AuthRequirement::DeviceOrOwnWorkerRecovery, PortStatus::AwaitingDomainPort),
];

/// health, DB export, metrics, audit, diagnostics, transcription, and the retired Connect Sync.
#[rustfmt::skip]
pub const ROWS_RPC: &[MethodRoute] = &[
    route("MiscFlags", "rpc", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("MiscHealth", "rpc", AuthRequirement::Public, PortStatus::Implemented),
    route("MiscDbExportUrl", "rpc", AuthRequirement::DeviceOnHost, PortStatus::Implemented),
    route("MiscMetrics", "rpc", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("AuditList", "rpc", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("TranscriptionGetConfig", "rpc", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("TranscriptionSetConfig", "rpc", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("TranscriptionGrantToken", "rpc", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("TranscriptionTest", "rpc", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("Sync", "rpc", AuthRequirement::Device, PortStatus::Implemented),
    route("DiagDebugLogBatch", "rpc", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("DiagSnapshot", "rpc", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
];

/// ui-cc typed state and layout application, both fenced to one socket generation.
#[rustfmt::skip]
pub const ROWS_UI_STATE: &[MethodRoute] = &[
    route("UiReportState", "ui_state", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("UiListStates", "ui_state", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("UiDispatch", "ui_state", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("UiApplyLayout", "ui_state", AuthRequirement::DevicePlusFence, PortStatus::AwaitingDomainPort),
];

/// web push subscriptions, per authenticated browser fingerprint.
#[rustfmt::skip]
pub const ROWS_PUSH: &[MethodRoute] = &[
    route("PushGetConfig", "push", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("PushSubscribe", "push", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("PushUnsubscribe", "push", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
];

/// Every per-domain table, in the order `method_route` concatenates them.
///
/// THE ORDER OF THIS LIST IS THE PROTO'S DECLARATION ORDER, and the coverage test
/// asserts it: a reordering here is a diff that buries a real change in noise.
#[rustfmt::skip]
pub const ALL_TABLES: &[&[MethodRoute]] = &[
    ROWS_WORKERS,
    ROWS_DEPLOY,
    ROWS_SESSIONS,
    ROWS_AGENTS,
    ROWS_SEARCH,
    ROWS_ATTACHMENTS,
    ROWS_AUTH,
    ROWS_RPC,
    ROWS_UI_STATE,
    ROWS_PUSH,
];
