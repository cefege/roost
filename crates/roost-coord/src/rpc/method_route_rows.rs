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
    route("WorkersList", "workers", AuthRequirement::Device, PortStatus::Implemented),
    route("WorkersRegister", "workers", AuthRequirement::Worker, PortStatus::Implemented),
    route("WorkersHeartbeat", "workers", AuthRequirement::Worker, PortStatus::Implemented),
    route("WorkersRename", "workers", AuthRequirement::Device, PortStatus::Implemented),
    route("WorkersDelete", "workers", AuthRequirement::Device, PortStatus::Implemented),
    route("WorkersDeployStart", "workers", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("WorkersDeployOutput", "workers", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
];

/// keeper-update preparation, the host-local change that replaces the binary every live PTY depends on.
///
/// `Device`, and NOT `DeviceOnHost`, which is what this row claimed until the
/// C1 integration read the handler rather than the plan. This is the one method
/// in the table where the convenient value and the enforced one differ in
/// *permission*, so it is worth the paragraph:
///
/// - the auth gate treats `Device` and `DeviceOnHost` identically
///   (`service.rs::principal_satisfies` answers `is_browser` for both), so the
///   "on host" half was never enforced at the gate;
/// - `deploy::keeper_update::handle_workers_prepare_keeper_update` calls
///   `require_account_device` and re-authorizes inside the drain, and never
///   reads `caller.on_host` (`keeper_update.rs:158`);
/// - v2's handler does the same — `handlers-workers-update.ts:83` is
///   `requireAccountDevice(ctx.values)` with no `assertOnHost`, unlike the four
///   pairing and device methods that do call it.
///
/// So `DeviceOnHost` was a claim no code in either tree provided, on the one
/// method that can replace the binary under every live PTY. The truthful value
/// is `Device`, and it matches v2. Recorded rather than quietly corrected
/// because a narrowing here would be a parity regression and a widening would
/// be a new one, and neither is mine to decide at an integration gate.
#[rustfmt::skip]
pub const ROWS_DEPLOY: &[MethodRoute] = &[route(
    "WorkersPrepareKeeperUpdate",
    "deploy",
    AuthRequirement::Device,
    PortStatus::Implemented,
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
    route("SessionsGetScrollbackCells", "sessions", AuthRequirement::Device, PortStatus::Implemented),
    route("SessionsSearchScrollback", "sessions", AuthRequirement::Device, PortStatus::Implemented),
    route("SessionsCancelScrollbackSearch", "sessions", AuthRequirement::Device, PortStatus::Implemented),
    route("SessionsGrantLocalTerminal", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("SessionsNegotiateLocalTerminalPeer", "sessions", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("WorkspacesList", "sessions", AuthRequirement::Device, PortStatus::Implemented),
    route("WorkspacesCreate", "sessions", AuthRequirement::Device, PortStatus::Implemented),
    route("WorkspacesUpdate", "sessions", AuthRequirement::Device, PortStatus::Implemented),
    route("WorkspacesDelete", "sessions", AuthRequirement::Device, PortStatus::Implemented),
    route("WorkspacesSetSessions", "sessions", AuthRequirement::Device, PortStatus::Implemented),
    route("TasksList", "sessions", AuthRequirement::Device, PortStatus::Implemented),
    route("TasksEnqueue", "sessions", AuthRequirement::Device, PortStatus::Implemented),
    route("TasksNextPending", "sessions", AuthRequirement::Device, PortStatus::Implemented),
    route("TasksSetState", "sessions", AuthRequirement::Device, PortStatus::Implemented),
    route("TasksCancel", "sessions", AuthRequirement::Device, PortStatus::Implemented),
    route("McpList", "sessions", AuthRequirement::Device, PortStatus::Implemented),
    route("McpCreate", "sessions", AuthRequirement::Device, PortStatus::Implemented),
    route("McpDelete", "sessions", AuthRequirement::Device, PortStatus::Implemented),
    route("McpPublish", "sessions", AuthRequirement::Device, PortStatus::Implemented),
];

/// agent status, the fenced agent prompt, and agent configuration.
///
/// The five status and config methods are `Device`, and that is what the
/// handler enforces: each opens with `require_account_device`
/// (`agents/rpc_status.rs:50, 68, 95, 137, 151`). The gate is right to leave
/// the fence out of them — a status read is scoped by the session id it names
/// and `handle_agent_status_wait` refuses an unopenable session before it
/// reveals whether one exists, which is the property
/// `a_wait_never_becomes_an_oracle_for_which_sessions_exist` pins. `DevicePlusFence`
/// would be a claim about a fence none of the five consults.
#[rustfmt::skip]
pub const ROWS_AGENTS: &[MethodRoute] = &[
    route("SessionsPrompt", "agents", AuthRequirement::DevicePlusFence, PortStatus::AwaitingDomainPort),
    route("AgentStatusGet", "agents", AuthRequirement::Device, PortStatus::Implemented),
    route("AgentStatusList", "agents", AuthRequirement::Device, PortStatus::Implemented),
    route("AgentStatusWait", "agents", AuthRequirement::Device, PortStatus::Implemented),
    route("AgentConfigGet", "agents", AuthRequirement::Device, PortStatus::Implemented),
    route("AgentConfigSet", "agents", AuthRequirement::Device, PortStatus::Implemented),
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
///
/// TWO OF THESE ROWS LOOK LIKE MISTAKES AND ARE NOT. Both were audited at the
/// C1 integration against the handler rather than against what reads well, and
/// the audit changed one value and confirmed the other four:
///
/// - **`PairList`, `PairApprove`, `PairDeny` and `PairApprovalStatus` keep
///   `DeviceOrOwnWorkerRecovery`, which reads as if a machine could approve a
///   browser.** It can reach the handler and the handler refuses it:
///   `auth/pairing/rpc_support.rs:84` admits a browser anywhere, or a
///   non-browser principal only when it arrived on this host, and
///   `tests/pairing_approver_gate.rs` pins BOTH directions — a remote worker is
///   refused, and the same worker on the host is admitted. That file is the
///   defence of this column; do not narrow it without a replacement for it.
/// - **`DevicesRevoke` is `Device`, and the handler's on-host arm is therefore
///   unreachable.** v2 lets an operator with no credential at all run it
///   (`handlers-devices.ts:71-72` — `if (!caller) assertOnHost(...)`), and the
///   port keeps that rule at `auth/rpc_devices.rs:143` so the handler is right
///   the moment the gate widens. Recorded here because this table is where an
///   auditor looks, and "the row says Device while the handler says Device OR
///   on-host" is exactly the kind of mismatch that reads as a lie in one
///   direction and a recovery path in the other. Neither exists: the gate
///   admits a browser and nothing else, today.
#[rustfmt::skip]
pub const ROWS_AUTH: &[MethodRoute] = &[
    route("AuthCoordIdentity", "auth", AuthRequirement::Public, PortStatus::AwaitingDomainPort),
    route("AuthDashboardAccess", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("AuthLogout", "auth", AuthRequirement::Device, PortStatus::Implemented),
    route("AuthOwnerActivate", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("AuthPasswordResetRequest", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("AuthPasswordResetRedeem", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("AuthPasswordLogin", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("AuthFederatedContinue", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("AuthCredentialsGet", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("AuthPasswordAdd", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("AuthFederatedLinkBegin", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("AuthFederatedLink", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("AuthMintBootstrap", "auth", AuthRequirement::Device, PortStatus::Implemented),
    route("AuthRedeemWorker", "auth", AuthRequirement::Public, PortStatus::Implemented),
    route("AuthRedeemBrowser", "auth", AuthRequirement::Public, PortStatus::Implemented),
    route("AuthMintCoordinatorRelocation", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("AuthRedeemCoordinatorRelocation", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("DevicesList", "auth", AuthRequirement::Device, PortStatus::Implemented),
    route("DevicesRevoke", "auth", AuthRequirement::Device, PortStatus::Implemented),
    route("DevicesRotateCurrent", "auth", AuthRequirement::Device, PortStatus::Implemented),
    route("CoordinatorMovePreflight", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("CoordinatorMoveStart", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("CoordinatorMoveStatus", "auth", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("PairCreate", "auth", AuthRequirement::Public, PortStatus::Implemented),
    route("PairPoll", "auth", AuthRequirement::Public, PortStatus::Implemented),
    route("PairList", "auth", AuthRequirement::DeviceOrOwnWorkerRecovery, PortStatus::Implemented),
    route("PairApprove", "auth", AuthRequirement::DeviceOrOwnWorkerRecovery, PortStatus::Implemented),
    route("PairConfirm", "auth", AuthRequirement::Public, PortStatus::Implemented),
    route("PairDeny", "auth", AuthRequirement::DeviceOrOwnWorkerRecovery, PortStatus::Implemented),
    route("PairApprovalStatus", "auth", AuthRequirement::DeviceOrOwnWorkerRecovery, PortStatus::Implemented),
];

/// health, DB export, metrics, audit, diagnostics, transcription, and the retired Connect Sync.
#[rustfmt::skip]
pub const ROWS_RPC: &[MethodRoute] = &[
    route("MiscFlags", "rpc", AuthRequirement::Unwired, PortStatus::UnwiredInV2),
    route("MiscHealth", "rpc", AuthRequirement::Public, PortStatus::Implemented),
    route("MiscDbExportUrl", "rpc", AuthRequirement::DeviceOnHost, PortStatus::Implemented),
    route("MiscMetrics", "rpc", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("AuditList", "rpc", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("TranscriptionGetConfig", "rpc", AuthRequirement::Device, PortStatus::Implemented),
    route("TranscriptionSetConfig", "rpc", AuthRequirement::Device, PortStatus::Implemented),
    route("TranscriptionGrantToken", "rpc", AuthRequirement::Device, PortStatus::Implemented),
    route("TranscriptionTest", "rpc", AuthRequirement::Device, PortStatus::Implemented),
    route("Sync", "rpc", AuthRequirement::Device, PortStatus::Implemented),
    route("DiagDebugLogBatch", "rpc", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
    route("DiagSnapshot", "rpc", AuthRequirement::Device, PortStatus::AwaitingDomainPort),
];

/// ui-cc typed state and layout application, both fenced to one socket generation.
///
/// ALL FOUR CARRY THE FENCE, not only the apply: `UiReportState`, `UiListStates`
/// and `UiDispatch` each call `require_tab_fence` before they read or write the
/// retained state (`apps/coord/src/ui-state/handlers-ui.ts`), and an apply
/// reserved against a socket that is not the one the caller holds is applied by
/// a tab that no longer owns it. The column records the requirement the
/// handler enforces, not the one that would have been convenient.
#[rustfmt::skip]
pub const ROWS_UI_STATE: &[MethodRoute] = &[
    route("UiReportState", "ui_state", AuthRequirement::DevicePlusFence, PortStatus::Implemented),
    route("UiListStates", "ui_state", AuthRequirement::DevicePlusFence, PortStatus::Implemented),
    route("UiDispatch", "ui_state", AuthRequirement::DevicePlusFence, PortStatus::Implemented),
    route("UiApplyLayout", "ui_state", AuthRequirement::DevicePlusFence, PortStatus::Implemented),
];

/// web push subscriptions, per authenticated browser fingerprint.
#[rustfmt::skip]
pub const ROWS_PUSH: &[MethodRoute] = &[
    route("PushGetConfig", "push", AuthRequirement::Device, PortStatus::Implemented),
    route("PushSubscribe", "push", AuthRequirement::Device, PortStatus::Implemented),
    route("PushUnsubscribe", "push", AuthRequirement::Device, PortStatus::Implemented),
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
