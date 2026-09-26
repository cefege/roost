//! The ONE `CoordinatorService` implementation: all 103 methods in one `impl`.
//!
//! Owned by the coordinator's RPC layer. This is the single service literal, and
//! it is ONE FILE because Rust does not allow one trait's impl to be split: two
//! `impl CoordinatorService for CoordinatorServiceImpl` blocks are E0119
//! `conflicting implementations` even when their method names are disjoint. That
//! is a language rule, not a style choice, and it was verified rather than
//! assumed.
//!
//! THE GUARANTEE IS STRONGER THAN THE ONE IT REPLACES. v2's hazard was a second
//! `router.service()` call silently shadowing every method Connect had not
//! registered (`apps/coord/src/rpc/router.ts:114-118`); here a method with no
//! delegation is a compile error, and there is exactly one block to shadow.
//!
//! The v2 domain split survives as a comment banner per domain and as
//! `method_route`'s `domain` column, which is what a per-domain handler needs to
//! find its own methods. Splitting the block would buy file size and cost the
//! guarantee, so the size goes to a recorded exception instead.
//!
//! Every signature is transcribed from the `service CoordinatorService` block in
//! `protocol/proto/roost/v1/coordinator.proto`, which is also what
//! `tests/method_route_coverage.rs` asserts the route table against.
//!
//! WIRED METHODS ARE NOT UNREACHABLE BEHIND A NAMED `Unimplemented`. Fifteen
//! arms are wired: the five worker methods (`WorkersList`, `WorkersRegister`,
//! `WorkersHeartbeat`, `WorkersRename`, `WorkersDelete`), the three scrollback
//! methods (`SessionsGetScrollbackCells`, `SessionsSearchScrollback`,
//! `SessionsCancelScrollbackSearch`), the four UI methods (`UiReportState`,
//! `UiListStates`, `UiDispatch`, `UiApplyLayout`) and the three push methods
//! (`PushGetConfig`, `PushSubscribe`, `PushUnsubscribe`). Each resolves its
//! caller through [`caller_of`](super::service::caller_of) and hands the owned
//! request to its domain handler, so wiring a method means naming its handler
//! here, and the refusal a wired method still answers with -- no caller on the
//! request -- stays a named `Unimplemented` rather than becoming an anonymous
//! success.

use std::future::Future;

use connectrpc::{
    Encodable, RequestContext, Response, ServiceRequest, ServiceResult, ServiceStream,
};
use roost_proto::roost::v1::CoordinatorService;
use roost_proto::*;

use super::service::{
    CoordinatorServiceImpl, caller_of, db_export_url, delegated_reply, delegated_stream,
    misc_health_reply, now_ms, sync_moved_stream,
};

use crate::agents::rpc_status::{
    handle_agent_config_get, handle_agent_config_set, handle_agent_status_get,
    handle_agent_status_list, handle_agent_status_wait,
};
use crate::auth::rpc_bootstrap::{
    handle_auth_mint_bootstrap, handle_auth_redeem_browser, handle_auth_redeem_worker,
};
use crate::auth::rpc_devices::{
    handle_auth_logout, handle_devices_list, handle_devices_revoke, handle_devices_rotate_current,
};
use crate::auth::rpc_pairing::{
    handle_pair_approval_status, handle_pair_approve, handle_pair_confirm, handle_pair_create,
    handle_pair_deny, handle_pair_list, handle_pair_poll,
};
use crate::deploy::keeper_update::handle_workers_prepare_keeper_update;
use crate::diagnostics::rpc_transcription::{
    handle_transcription_get_config, handle_transcription_grant_token,
    handle_transcription_set_config, handle_transcription_test,
};
use crate::push::rpc::{handle_push_get_config, handle_push_subscribe, handle_push_unsubscribe};
use crate::sessions::mcp::{
    handle_mcp_create, handle_mcp_delete, handle_mcp_list, handle_mcp_publish,
};
use crate::sessions::rpc_workspaces::{
    handle_workspaces_create, handle_workspaces_delete, handle_workspaces_list,
    handle_workspaces_set_sessions, handle_workspaces_update,
};
use crate::sessions::tasks::{
    handle_tasks_cancel, handle_tasks_enqueue, handle_tasks_list, handle_tasks_next_pending,
    handle_tasks_set_state,
};
use crate::terminal_screen::rpc::{
    handle_sessions_cancel_scrollback_search, handle_sessions_get_scrollback_cells,
    handle_sessions_search_scrollback,
};
use crate::ui_state::rpc::{
    handle_ui_apply_layout, handle_ui_dispatch, handle_ui_list_states, handle_ui_report_state,
};
use crate::workers::rpc::{
    handle_workers_delete, handle_workers_heartbeat, handle_workers_list, handle_workers_register,
    handle_workers_rename,
};

// `async fn` is not an option on any arm here: the generated trait declares
// `-> impl Future<Output = ...> + Send`, and an `async fn` desugars to an opaque
// with no `+ Send`, which does not match. The `async move` block is the same
// future, written so the bound is present.
#[allow(clippy::manual_async_fn)]
impl CoordinatorService for CoordinatorServiceImpl {
    // ── workers ─────────────────────────────────────────────────────────

    fn workers_list<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, WorkersListRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<WorkersListResponse> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "WorkersList")?;
            handle_workers_list(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn workers_register<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, WorkersRegisterRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<WorkersRegisterResponse> + Send + use<'a>>,
    > + Send {
        async move {
            let caller = caller_of(&ctx, "WorkersRegister")?;
            handle_workers_register(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn workers_heartbeat<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, WorkersHeartbeatRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<WorkersHeartbeatResponse> + Send + use<'a>>,
    > + Send {
        async move {
            let caller = caller_of(&ctx, "WorkersHeartbeat")?;
            handle_workers_heartbeat(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn workers_rename<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, WorkersRenameRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<WorkersRenameResponse> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "WorkersRename")?;
            handle_workers_rename(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn workers_delete<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, WorkersDeleteRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<WorkersDeleteResponse> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "WorkersDelete")?;
            handle_workers_delete(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn workers_deploy_start<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, WorkersDeployStartRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<WorkersDeployStartResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<WorkersDeployStartResponse>("WorkersDeployStart")
    }

    fn workers_deploy_output(
        &self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, WorkersDeployOutputRequest>,
    ) -> impl Future<
        Output = ServiceResult<
            ServiceStream<impl Encodable<WorkersDeployOutputFrame> + Send + use<>>,
        >,
    > + Send {
        delegated_stream::<WorkersDeployOutputFrame>("WorkersDeployOutput")
    }
    // ── deploy ──────────────────────────────────────────────────────────

    fn workers_prepare_keeper_update<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, WorkersPrepareKeeperUpdateRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<WorkersPrepareKeeperUpdateResponse> + Send + use<'a>>,
    > + Send {
        async move {
            let caller = caller_of(&ctx, "WorkersPrepareKeeperUpdate")?;
            handle_workers_prepare_keeper_update(&self.core, caller, r.to_owned_message()).await
        }
    }
    // ── sessions ────────────────────────────────────────────────────────

    fn sessions_list<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, SessionsListRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<SessionsListResponse> + Send + use<'a>>> + Send
    {
        delegated_reply::<SessionsListResponse>("SessionsList")
    }

    fn sessions_spawn<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, SessionsSpawnRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<SessionsSpawnResponse> + Send + use<'a>>> + Send
    {
        delegated_reply::<SessionsSpawnResponse>("SessionsSpawn")
    }

    fn sessions_attach<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, SessionsAttachRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<SessionsAttachResponse> + Send + use<'a>>>
    + Send {
        delegated_reply::<SessionsAttachResponse>("SessionsAttach")
    }

    fn sessions_kill<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, SessionsKillRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<SessionsKillResponse> + Send + use<'a>>> + Send
    {
        delegated_reply::<SessionsKillResponse>("SessionsKill")
    }

    fn sessions_rename<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, SessionsRenameRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<SessionsRenameResponse> + Send + use<'a>>>
    + Send {
        delegated_reply::<SessionsRenameResponse>("SessionsRename")
    }

    fn sessions_input<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, SessionsInputRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<SessionsInputResponse> + Send + use<'a>>> + Send
    {
        delegated_reply::<SessionsInputResponse>("SessionsInput")
    }
    // ── agents ──────────────────────────────────────────────────────────

    fn sessions_prompt<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, SessionsPromptRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<SessionsPromptResponse> + Send + use<'a>>>
    + Send {
        delegated_reply::<SessionsPromptResponse>("SessionsPrompt")
    }
    // ── sessions ────────────────────────────────────────────────────────

    fn sessions_cursor_pos<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, SessionsCursorPosRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<SessionsCursorPosResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<SessionsCursorPosResponse>("SessionsCursorPos")
    }

    fn sessions_assign_workspace<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, SessionsAssignWorkspaceRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<SessionsAssignWorkspaceResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<SessionsAssignWorkspaceResponse>("SessionsAssignWorkspace")
    }

    fn sessions_get_scrollback_cells<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, SessionsGetScrollbackCellsRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<SessionsGetScrollbackCellsResponse> + Send + use<'a>>,
    > + Send {
        async move {
            let caller = caller_of(&ctx, "SessionsGetScrollbackCells")?;
            handle_sessions_get_scrollback_cells(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn sessions_search_scrollback<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, SessionsSearchScrollbackRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<SessionsSearchScrollbackResponse> + Send + use<'a>>,
    > + Send {
        async move {
            let caller = caller_of(&ctx, "SessionsSearchScrollback")?;
            handle_sessions_search_scrollback(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn sessions_cancel_scrollback_search<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, SessionsCancelScrollbackSearchRequest>,
    ) -> impl Future<
        Output = ServiceResult<
            impl Encodable<SessionsCancelScrollbackSearchResponse> + Send + use<'a>,
        >,
    > + Send {
        async move {
            let caller = caller_of(&ctx, "SessionsCancelScrollbackSearch")?;
            handle_sessions_cancel_scrollback_search(&self.core, caller, r.to_owned_message()).await
        }
    }
    // ── search ──────────────────────────────────────────────────────────

    fn sessions_search_global<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, SessionsSearchGlobalRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<SessionsSearchGlobalResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<SessionsSearchGlobalResponse>("SessionsSearchGlobal")
    }

    fn sessions_cancel_global_search<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, SessionsCancelGlobalSearchRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<SessionsCancelGlobalSearchResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<SessionsCancelGlobalSearchResponse>("SessionsCancelGlobalSearch")
    }
    // ── sessions ────────────────────────────────────────────────────────

    fn sessions_grant_local_terminal<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, SessionsGrantLocalTerminalRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<SessionsGrantLocalTerminalResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<SessionsGrantLocalTerminalResponse>("SessionsGrantLocalTerminal")
    }

    fn sessions_negotiate_local_terminal_peer<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, SessionsNegotiateLocalTerminalPeerRequest>,
    ) -> impl Future<
        Output = ServiceResult<
            impl Encodable<SessionsNegotiateLocalTerminalPeerResponse> + Send + use<'a>,
        >,
    > + Send {
        delegated_reply::<SessionsNegotiateLocalTerminalPeerResponse>(
            "SessionsNegotiateLocalTerminalPeer",
        )
    }
    // ── attachments ─────────────────────────────────────────────────────

    fn sessions_negotiate_attachment_peer<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, SessionsNegotiateAttachmentPeerRequest>,
    ) -> impl Future<
        Output = ServiceResult<
            impl Encodable<SessionsNegotiateAttachmentPeerResponse> + Send + use<'a>,
        >,
    > + Send {
        delegated_reply::<SessionsNegotiateAttachmentPeerResponse>(
            "SessionsNegotiateAttachmentPeer",
        )
    }
    // ── agents ──────────────────────────────────────────────────────────

    fn agent_status_get<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, AgentStatusGetRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<AgentStatusGetResponse> + Send + use<'a>>>
    + Send {
        async move {
            let caller = caller_of(&ctx, "AgentStatusGet")?;
            handle_agent_status_get(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn agent_status_list<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, AgentStatusListRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<AgentStatusListResponse> + Send + use<'a>>,
    > + Send {
        async move {
            let caller = caller_of(&ctx, "AgentStatusList")?;
            handle_agent_status_list(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn agent_status_wait<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, AgentStatusWaitRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<AgentStatusWaitResponse> + Send + use<'a>>,
    > + Send {
        async move {
            let caller = caller_of(&ctx, "AgentStatusWait")?;
            handle_agent_status_wait(&self.core, caller, r.to_owned_message()).await
        }
    }
    // ── sessions ────────────────────────────────────────────────────────

    fn workspaces_list<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, WorkspacesListRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<WorkspacesListResponse> + Send + use<'a>>>
    + Send {
        async move {
            let caller = caller_of(&ctx, "WorkspacesList")?;
            handle_workspaces_list(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn workspaces_create<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, WorkspacesCreateRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<WorkspacesCreateResponse> + Send + use<'a>>,
    > + Send {
        async move {
            let caller = caller_of(&ctx, "WorkspacesCreate")?;
            handle_workspaces_create(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn workspaces_update<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, WorkspacesUpdateRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<WorkspacesUpdateResponse> + Send + use<'a>>,
    > + Send {
        async move {
            let caller = caller_of(&ctx, "WorkspacesUpdate")?;
            handle_workspaces_update(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn workspaces_delete<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, WorkspacesDeleteRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<WorkspacesDeleteResponse> + Send + use<'a>>,
    > + Send {
        async move {
            let caller = caller_of(&ctx, "WorkspacesDelete")?;
            handle_workspaces_delete(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn workspaces_set_sessions<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, WorkspacesSetSessionsRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<WorkspacesSetSessionsResponse> + Send + use<'a>>,
    > + Send {
        async move {
            let caller = caller_of(&ctx, "WorkspacesSetSessions")?;
            handle_workspaces_set_sessions(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn tasks_list<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, TasksListRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<TasksListResponse> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "TasksList")?;
            handle_tasks_list(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn tasks_enqueue<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, TasksEnqueueRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<TasksEnqueueResponse> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "TasksEnqueue")?;
            handle_tasks_enqueue(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn tasks_next_pending<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, TasksNextPendingRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<TasksNextPendingResponse> + Send + use<'a>>,
    > + Send {
        async move {
            let caller = caller_of(&ctx, "TasksNextPending")?;
            handle_tasks_next_pending(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn tasks_set_state<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, TasksSetStateRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<TasksSetStateResponse> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "TasksSetState")?;
            handle_tasks_set_state(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn tasks_cancel<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, TasksCancelRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<TasksCancelResponse> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "TasksCancel")?;
            handle_tasks_cancel(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn mcp_list<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, McpListRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<McpListResponse> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "McpList")?;
            handle_mcp_list(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn mcp_create<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, McpCreateRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<McpCreateResponse> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "McpCreate")?;
            handle_mcp_create(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn mcp_delete<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, McpDeleteRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<McpDeleteResponse> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "McpDelete")?;
            handle_mcp_delete(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn mcp_publish<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, McpPublishRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<McpPublishResponse> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "McpPublish")?;
            handle_mcp_publish(&self.core, caller, r.to_owned_message()).await
        }
    }
    // ── auth ────────────────────────────────────────────────────────────

    fn auth_coord_identity<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, AuthCoordIdentityRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<AuthCoordIdentityResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<AuthCoordIdentityResponse>("AuthCoordIdentity")
    }

    fn auth_mint_bootstrap<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, AuthMintBootstrapRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<AuthMintBootstrapResponse> + Send + use<'a>>,
    > + Send {
        async move {
            let caller = caller_of(&ctx, "AuthMintBootstrap")?;
            handle_auth_mint_bootstrap(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn auth_redeem_worker<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, AuthRedeemWorkerRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<AuthRedeemWorkerResponse> + Send + use<'a>>,
    > + Send {
        async move {
            let caller = caller_of(&ctx, "AuthRedeemWorker")?;
            handle_auth_redeem_worker(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn auth_redeem_browser<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, AuthRedeemBrowserRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<AuthRedeemBrowserResponse> + Send + use<'a>>,
    > + Send {
        async move {
            let caller = caller_of(&ctx, "AuthRedeemBrowser")?;
            handle_auth_redeem_browser(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn auth_logout<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, AuthLogoutRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<AuthLogoutResponse> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "AuthLogout")?;
            handle_auth_logout(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn pair_create<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, PairCreateRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<PairCreateResponse> + Send + use<'a>>> + Send
    {
        async move { handle_pair_create(&self.core, &ctx, r.to_owned_message()).await }
    }

    fn pair_poll<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, PairPollRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<PairPollResponse> + Send + use<'a>>> + Send
    {
        async move { handle_pair_poll(&self.core, &ctx, r.to_owned_message()).await }
    }

    fn pair_list<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, PairListRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<PairListResponse> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "PairList")?;
            handle_pair_list(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn pair_approve<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, PairApproveRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<PairApproveResponse> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "PairApprove")?;
            handle_pair_approve(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn pair_confirm<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, PairConfirmRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<PairConfirmResponse> + Send + use<'a>>> + Send
    {
        async move { handle_pair_confirm(&self.core, &ctx, r.to_owned_message()).await }
    }

    fn pair_deny<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, PairDenyRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<PairDenyResponse> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "PairDeny")?;
            handle_pair_deny(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn pair_approval_status<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, PairApprovalStatusRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<PairApprovalStatusResponse> + Send + use<'a>>,
    > + Send {
        async move {
            let caller = caller_of(&ctx, "PairApprovalStatus")?;
            handle_pair_approval_status(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn devices_list<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, DevicesListRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<DevicesListResponse> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "DevicesList")?;
            handle_devices_list(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn devices_revoke<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, DevicesRevokeRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<DevicesRevokeResponse> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "DevicesRevoke")?;
            handle_devices_revoke(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn devices_rotate_current<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, DevicesRotateCurrentRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<DevicesRotateCurrentResponse> + Send + use<'a>>,
    > + Send {
        async move {
            let caller = caller_of(&ctx, "DevicesRotateCurrent")?;
            handle_devices_rotate_current(&self.core, caller, r.to_owned_message()).await
        }
    }
    // ── rpc ─────────────────────────────────────────────────────────────

    fn misc_health<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, MiscHealthRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<MiscHealthResponse> + Send + use<'a>>> + Send
    {
        async move { Response::ok(misc_health_reply(self, now_ms())) }
    }

    fn misc_db_export_url<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, MiscDbExportUrlRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<MiscDbExportUrlResponse> + Send + use<'a>>,
    > + Send {
        async move {
            Response::ok(MiscDbExportUrlResponse {
                url: db_export_url(self),
                ..Default::default()
            })
        }
    }

    fn misc_metrics<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, MiscMetricsRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<MiscMetricsResponse> + Send + use<'a>>> + Send
    {
        delegated_reply::<MiscMetricsResponse>("MiscMetrics")
    }

    fn audit_list<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, AuditListRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<AuditListResponse> + Send + use<'a>>> + Send
    {
        delegated_reply::<AuditListResponse>("AuditList")
    }

    fn diag_debug_log_batch<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, DiagDebugLogBatchRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<DiagDebugLogBatchResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<DiagDebugLogBatchResponse>("DiagDebugLogBatch")
    }

    fn diag_snapshot<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, DiagSnapshotRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<DiagSnapshotResponse> + Send + use<'a>>> + Send
    {
        delegated_reply::<DiagSnapshotResponse>("DiagSnapshot")
    }
    // ── attachments ─────────────────────────────────────────────────────

    fn files_read<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, FilesReadRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<FilesReadResponse> + Send + use<'a>>> + Send
    {
        delegated_reply::<FilesReadResponse>("FilesRead")
    }

    fn files_read_chunk<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, FilesReadChunkRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<FilesReadChunkResponse> + Send + use<'a>>>
    + Send {
        delegated_reply::<FilesReadChunkResponse>("FilesReadChunk")
    }

    fn files_list_dir<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, FilesListDirRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<FilesListDirResponse> + Send + use<'a>>> + Send
    {
        delegated_reply::<FilesListDirResponse>("FilesListDir")
    }

    fn files_mkdir<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, FilesMkdirRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<FilesMkdirResponse> + Send + use<'a>>> + Send
    {
        delegated_reply::<FilesMkdirResponse>("FilesMkdir")
    }

    fn attach_file_chunk<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, AttachFileChunkRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<AttachFileChunkResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<AttachFileChunkResponse>("AttachFileChunk")
    }

    fn attachment_probe<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, AttachmentProbeRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<AttachmentProbeResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<AttachmentProbeResponse>("AttachmentProbe")
    }

    fn list_attachments<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, ListAttachmentsRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<ListAttachmentsResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<ListAttachmentsResponse>("ListAttachments")
    }

    fn delete_attachment<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, DeleteAttachmentRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<DeleteAttachmentResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<DeleteAttachmentResponse>("DeleteAttachment")
    }

    fn attachments_grant_direct<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, AttachmentsGrantDirectRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<AttachmentsGrantDirectResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<AttachmentsGrantDirectResponse>("AttachmentsGrantDirect")
    }

    fn attachments_direct_status<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, AttachmentsDirectStatusRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<AttachmentsDirectStatusResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<AttachmentsDirectStatusResponse>("AttachmentsDirectStatus")
    }
    // ── rpc ─────────────────────────────────────────────────────────────

    fn transcription_get_config<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, TranscriptionGetConfigRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<TranscriptionConfig> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "TranscriptionGetConfig")?;
            handle_transcription_get_config(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn transcription_set_config<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, TranscriptionSetConfigRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<TranscriptionConfig> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "TranscriptionSetConfig")?;
            handle_transcription_set_config(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn transcription_grant_token<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, TranscriptionGrantTokenRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<TranscriptionGrantTokenResponse> + Send + use<'a>>,
    > + Send {
        async move {
            let caller = caller_of(&ctx, "TranscriptionGrantToken")?;
            handle_transcription_grant_token(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn transcription_test<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, TranscriptionTestRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<TranscriptionTestResponse> + Send + use<'a>>,
    > + Send {
        async move {
            let caller = caller_of(&ctx, "TranscriptionTest")?;
            handle_transcription_test(&self.core, caller, r.to_owned_message()).await
        }
    }
    // ── agents ──────────────────────────────────────────────────────────

    fn agent_config_get<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, AgentConfigGetRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<AgentConfig> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "AgentConfigGet")?;
            handle_agent_config_get(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn agent_config_set<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, AgentConfigSetRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<AgentConfig> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "AgentConfigSet")?;
            handle_agent_config_set(&self.core, caller, r.to_owned_message()).await
        }
    }
    // ── ui_state ────────────────────────────────────────────────────────

    fn ui_report_state<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, UiReportStateRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<UiReportStateResponse> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "UiReportState")?;
            handle_ui_report_state(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn ui_list_states<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, UiListStatesRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<UiListStatesResponse> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "UiListStates")?;
            handle_ui_list_states(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn ui_dispatch<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, UiDispatchRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<UiDispatchResponse> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "UiDispatch")?;
            handle_ui_dispatch(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn ui_apply_layout<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, UiApplyLayoutRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<UiApplyLayoutResponse> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "UiApplyLayout")?;
            handle_ui_apply_layout(&self.core, caller, r.to_owned_message()).await
        }
    }
    // ── push ────────────────────────────────────────────────────────────

    fn push_get_config<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, PushGetConfigRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<PushGetConfigResponse> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "PushGetConfig")?;
            handle_push_get_config(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn push_subscribe<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, PushSubscribeRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<PushSubscribeResponse> + Send + use<'a>>> + Send
    {
        async move {
            let caller = caller_of(&ctx, "PushSubscribe")?;
            handle_push_subscribe(&self.core, caller, r.to_owned_message()).await
        }
    }

    fn push_unsubscribe<'a>(
        &'a self,
        ctx: RequestContext,
        r: ServiceRequest<'_, PushUnsubscribeRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<PushUnsubscribeResponse> + Send + use<'a>>,
    > + Send {
        async move {
            let caller = caller_of(&ctx, "PushUnsubscribe")?;
            handle_push_unsubscribe(&self.core, caller, r.to_owned_message()).await
        }
    }
    // ── rpc ─────────────────────────────────────────────────────────────

    fn sync(
        &self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, SyncRequest>,
    ) -> impl Future<
        Output = ServiceResult<ServiceStream<impl Encodable<FirehoseFrame> + Send + use<>>>,
    > + Send {
        sync_moved_stream::<FirehoseFrame>()
    }
    // ── auth ────────────────────────────────────────────────────────────

    fn auth_dashboard_access<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, AuthDashboardAccessRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<AuthDashboardAccessResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<AuthDashboardAccessResponse>("AuthDashboardAccess")
    }

    fn auth_owner_activate<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, AuthOwnerActivateRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<AuthOwnerActivateResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<AuthOwnerActivateResponse>("AuthOwnerActivate")
    }

    fn auth_password_reset_request<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, AuthPasswordResetStartRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<AuthPasswordResetStartResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<AuthPasswordResetStartResponse>("AuthPasswordResetRequest")
    }

    fn auth_password_reset_redeem<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, AuthPasswordResetRedeemRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<AuthPasswordResetRedeemResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<AuthPasswordResetRedeemResponse>("AuthPasswordResetRedeem")
    }

    fn auth_password_login<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, AuthPasswordLoginRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<AuthPasswordLoginResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<AuthPasswordLoginResponse>("AuthPasswordLogin")
    }

    fn auth_federated_continue<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, AuthFederatedContinueRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<AuthFederatedContinueResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<AuthFederatedContinueResponse>("AuthFederatedContinue")
    }

    fn auth_credentials_get<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, AuthCredentialsGetRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<AuthCredentialsGetResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<AuthCredentialsGetResponse>("AuthCredentialsGet")
    }

    fn auth_password_add<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, AuthPasswordAddRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<AuthPasswordAddResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<AuthPasswordAddResponse>("AuthPasswordAdd")
    }

    fn auth_federated_link_begin<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, AuthFederatedLinkBeginRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<AuthFederatedLinkBeginResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<AuthFederatedLinkBeginResponse>("AuthFederatedLinkBegin")
    }

    fn auth_federated_link<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, AuthFederatedLinkRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<AuthFederatedLinkResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<AuthFederatedLinkResponse>("AuthFederatedLink")
    }

    fn auth_mint_coordinator_relocation<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, AuthMintCoordinatorRelocationRequest>,
    ) -> impl Future<
        Output = ServiceResult<
            impl Encodable<AuthMintCoordinatorRelocationResponse> + Send + use<'a>,
        >,
    > + Send {
        delegated_reply::<AuthMintCoordinatorRelocationResponse>("AuthMintCoordinatorRelocation")
    }

    fn auth_redeem_coordinator_relocation<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, AuthRedeemCoordinatorRelocationRequest>,
    ) -> impl Future<
        Output = ServiceResult<
            impl Encodable<AuthRedeemCoordinatorRelocationResponse> + Send + use<'a>,
        >,
    > + Send {
        delegated_reply::<AuthRedeemCoordinatorRelocationResponse>(
            "AuthRedeemCoordinatorRelocation",
        )
    }

    fn coordinator_move_preflight<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, CoordinatorMovePreflightRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<CoordinatorMovePreflightResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<CoordinatorMovePreflightResponse>("CoordinatorMovePreflight")
    }

    fn coordinator_move_start<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, CoordinatorMoveStartRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<CoordinatorMoveStartResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<CoordinatorMoveStartResponse>("CoordinatorMoveStart")
    }

    fn coordinator_move_status<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, CoordinatorMoveStatusRequest>,
    ) -> impl Future<
        Output = ServiceResult<impl Encodable<CoordinatorMoveStatusResponse> + Send + use<'a>>,
    > + Send {
        delegated_reply::<CoordinatorMoveStatusResponse>("CoordinatorMoveStatus")
    }
    // ── rpc ─────────────────────────────────────────────────────────────

    fn misc_flags<'a>(
        &'a self,
        _ctx: RequestContext,
        _r: ServiceRequest<'_, MiscFlagsRequest>,
    ) -> impl Future<Output = ServiceResult<impl Encodable<MiscFlagsResponse> + Send + use<'a>>> + Send
    {
        delegated_reply::<MiscFlagsResponse>("MiscFlags")
    }
}
