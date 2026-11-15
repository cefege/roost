//! The per-domain method tables behind `METHOD_ROUTES`, split by v2's own
//! `apps/coord/src/<domain>/` folders so each table reads on its own.
//!
//! The tables are concatenated in the proto's declaration order, which is what
//! `tests/method_route_coverage.rs` asserts -- so the split costs navigability
//! nothing and the ordering guarantee survives it.
//!
//! Kept as plain data rather than generated: 78 rows of cited policy is exactly
//! what a build-time macro would hide.
//!
//! `#[rustfmt::skip]` on every table, deliberately. This is DATA, and rustfmt
//! reformats one `route(...)` call to six lines, which turns a readable 200-line
//! table into 700 lines of ceremony and pushes the file over the size cap for no
//! gain. The one thing formatting buys here -- aligned columns a reader can scan
//! -- is the one thing this file most needs, and `#[rustfmt::skip]` is how a
//! data table keeps it.

use super::method_route::{AuthRequirement, MethodRoute};

const fn route(method: &'static str, domain: &'static str, auth: AuthRequirement) -> MethodRoute {
    MethodRoute {
        method,
        domain,
        auth,
    }
}

/// the worker registry: registration, heartbeat, rename, delete, deploy.
#[rustfmt::skip]
pub const ROWS_WORKERS: &[MethodRoute] = &[
    route("WorkersList", "workers", AuthRequirement::Device),
    route("WorkersRegister", "workers", AuthRequirement::Worker),
    route("WorkersHeartbeat", "workers", AuthRequirement::Worker),
    route("WorkersRename", "workers", AuthRequirement::Device),
    route("WorkersDelete", "workers", AuthRequirement::Device),
    route("WorkersDeployStart", "workers", AuthRequirement::Device),
    route("WorkersDeployOutput", "workers", AuthRequirement::Device),
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
)];

/// the session lifecycle, scrollback, and direct-terminal grants.
#[rustfmt::skip]
pub const ROWS_SESSIONS: &[MethodRoute] = &[
    route("SessionsList", "sessions", AuthRequirement::DeviceOrOwnWorkerRecovery),
    route("SessionsSpawn", "sessions", AuthRequirement::Device),
    route("SessionsAttach", "sessions", AuthRequirement::Device),
    route("SessionsKill", "sessions", AuthRequirement::Device),
    route("SessionsRename", "sessions", AuthRequirement::Device),
    route("SessionsInput", "sessions", AuthRequirement::Device),
    route("SessionsCursorPos", "sessions", AuthRequirement::Device),
    route("SessionsAssignWorkspace", "sessions", AuthRequirement::Device),
    route("SessionsGetScrollbackCells", "sessions", AuthRequirement::Device),
    route("SessionsGetTerminalImage", "sessions", AuthRequirement::Device),
    route("SessionsSearchScrollback", "sessions", AuthRequirement::Device),
    route("SessionsCancelScrollbackSearch", "sessions", AuthRequirement::Device),
    route("SessionsGrantLocalTerminal", "sessions", AuthRequirement::Device),
    route("SessionsNegotiateLocalTerminalPeer", "sessions", AuthRequirement::Device),
    route("WorkspacesList", "sessions", AuthRequirement::Device),
    route("WorkspacesCreate", "sessions", AuthRequirement::Device),
    route("WorkspacesUpdate", "sessions", AuthRequirement::Device),
    route("WorkspacesDelete", "sessions", AuthRequirement::Device),
    route("WorkspacesSetSessions", "sessions", AuthRequirement::Device),
];

/// global session search across every worker.
#[rustfmt::skip]
pub const ROWS_SEARCH: &[MethodRoute] = &[
    route("SessionsSearchGlobal", "search", AuthRequirement::Device),
    route("SessionsCancelGlobalSearch", "search", AuthRequirement::Device),
];

/// files, the attachment chunk relay, direct grants, and peer negotiation.
#[rustfmt::skip]
pub const ROWS_ATTACHMENTS: &[MethodRoute] = &[
    route("SessionsNegotiateAttachmentPeer", "attachments", AuthRequirement::DevicePlusFence),
    route("FilesRead", "attachments", AuthRequirement::Device),
    route("FilesReadChunk", "attachments", AuthRequirement::Device),
    route("FilesListDir", "attachments", AuthRequirement::Device),
    route("FilesMkdir", "attachments", AuthRequirement::Device),
    route("AttachmentsGrantDirect", "attachments", AuthRequirement::DevicePlusFence),
    route("AttachmentsDirectStatus", "attachments", AuthRequirement::DevicePlusFence),
    route("AttachFileChunk", "attachments", AuthRequirement::Device),
    route("AttachmentProbe", "attachments", AuthRequirement::Device),
    route("ListAttachments", "attachments", AuthRequirement::Device),
    route("DeleteAttachment", "attachments", AuthRequirement::Device),
];

/// Coordinator-backed clipboard history for paired browsers.
#[rustfmt::skip]
pub const ROWS_CLIPBOARD: &[MethodRoute] = &[
    route("ClipboardList", "clipboard", AuthRequirement::Device),
    route("ClipboardAdd", "clipboard", AuthRequirement::Device),
    route("ClipboardDelete", "clipboard", AuthRequirement::Device),
    route("ClipboardClear", "clipboard", AuthRequirement::Device),
];

/// Account-scoped browser state that follows the operator across devices.
#[rustfmt::skip]
pub const ROWS_CLIENT_STATE: &[MethodRoute] = &[
    route("ClientStateList", "client_state", AuthRequirement::Device),
    route("ClientStateSet", "client_state", AuthRequirement::Device),
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
    route("AuthCoordIdentity", "auth", AuthRequirement::Public),
    route("AuthLogout", "auth", AuthRequirement::Device),
    route("AuthMintBootstrap", "auth", AuthRequirement::Device),
    route("AuthRedeemWorker", "auth", AuthRequirement::Public),
    route("AuthRedeemBrowser", "auth", AuthRequirement::Public),
    route("DevicesList", "auth", AuthRequirement::Device),
    route("DevicesRevoke", "auth", AuthRequirement::Device),
    route("DevicesRotateCurrent", "auth", AuthRequirement::Device),
    route("PairCreate", "auth", AuthRequirement::Public),
    route("PairPoll", "auth", AuthRequirement::Public),
    route("PairList", "auth", AuthRequirement::DeviceOrOwnWorkerRecovery),
    route("PairApprove", "auth", AuthRequirement::DeviceOrOwnWorkerRecovery),
    route("PairConfirm", "auth", AuthRequirement::Public),
    route("PairDeny", "auth", AuthRequirement::DeviceOrOwnWorkerRecovery),
    route("PairApprovalStatus", "auth", AuthRequirement::DeviceOrOwnWorkerRecovery),
];

/// health, DB export, metrics, audit, diagnostics, and transcription.
#[rustfmt::skip]
pub const ROWS_RPC: &[MethodRoute] = &[
    route("MiscHealth", "rpc", AuthRequirement::Public),
    // `DeviceOnHost`, AND THE LOCALITY IS ENFORCED ONE LAYER BELOW THE GATE.
    // Re-read against the code rather than the plan, because the export route
    // stopped being a stub and this row is the only place that says what it
    // costs:
    //
    // - v2 asserts locality in the HANDLER, unguarded
    //   (`handlers-system.ts:114-115`): `requireAccountDevice` AND
    //   `assertOnHost`, so a remote browser is refused the URL itself.
    // - `service::principal_satisfies` answers `is_browser` for this
    //   variant and for `Device` alike, so the auth GATE still does not
    //   enforce locality, and a remote browser is still handed the path.
    // - the route behind that path enforces it in full: the on-host profile is
    //   resolved from the caller-origin layer and anything that is not on this
    //   host is refused `403` BEFORE a snapshot is taken
    //   (`http/listener.rs:258`). The database copy is streamed only to a
    //   caller that passed that check.
    //
    // So what a remote browser can learn from this row is a path string, and
    // what the gate does not do is hide that string. Narrowing the row to
    // `Device` would make the table agree with the gate by recording LESS
    // than v2 requires — an under-claiming row is the same defect as an
    // over-claiming one, and the fix is a gate change, not a table change.
    // `no_row_claims_a_locality_the_auth_gate_does_not_enforce` is the test
    // that holds both halves of this sentence.
    route("MiscDbExportUrl", "rpc", AuthRequirement::DeviceOnHost),
    route("MiscMetrics", "rpc", AuthRequirement::Device),
    route("AuditList", "rpc", AuthRequirement::Device),
    route("TranscriptionGetConfig", "rpc", AuthRequirement::Device),
    route("TranscriptionSetConfig", "rpc", AuthRequirement::Device),
    route("TranscriptionGrantToken", "rpc", AuthRequirement::Device),
    route("TranscriptionTest", "rpc", AuthRequirement::Device),
    route("DiagDebugLogBatch", "rpc", AuthRequirement::Device),
    route("DiagSnapshot", "rpc", AuthRequirement::Device),
];

/// ui-cc typed state and layout application.
///
/// ALL FOUR TAKE THE DEVICE PRINCIPAL ONLY. v2 fences none of them:
/// `apps/coord/src/ui-state/handlers-ui.ts:91,148,173,214` call only
/// `requireAccountDevice`, and the tab a method acts on is the request body's
/// `tabId`/`targetTabId`, never the caller's own tab header. The apply's
/// socket-generation fence is the live reservation it takes on the TARGET tab,
/// not a requirement on the caller.
#[rustfmt::skip]
pub const ROWS_UI_STATE: &[MethodRoute] = &[
    route("UiReportState", "ui_state", AuthRequirement::Device),
    route("UiListStates", "ui_state", AuthRequirement::Device),
    route("UiDispatch", "ui_state", AuthRequirement::Device),
    route("UiApplyLayout", "ui_state", AuthRequirement::Device),
];

/// web push subscriptions, per authenticated browser fingerprint.
#[rustfmt::skip]
pub const ROWS_PUSH: &[MethodRoute] = &[
    route("PushGetConfig", "push", AuthRequirement::Device),
    route("PushSubscribe", "push", AuthRequirement::Device),
    route("PushUnsubscribe", "push", AuthRequirement::Device),
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
    ROWS_SEARCH,
    ROWS_ATTACHMENTS,
    ROWS_CLIPBOARD,
    ROWS_CLIENT_STATE,
    ROWS_AUTH,
    ROWS_RPC,
    ROWS_UI_STATE,
    ROWS_PUSH,
];
