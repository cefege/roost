//! Opening and releasing one Sync socket: the revocation and reauth fences,
//! the link built over the socket's scope, the feed installed before the
//! `subscribed` barrier escapes, and a release that frees every owner in one
//! place.
//!
//! Called only by `sync_ws::socket`, which owns the loop between the two.
//! Ports `open` and `cleanupSocket` of `apps/coord/src/sync/sync-ws-handler.ts`
//! and `createSyncV2SocketState` of `sync-ws-v2-state.ts`.
//!
//! THE FEED IS LISTENING BEFORE THE BARRIER GOES OUT. A frame published between
//! `subscribed` and the first listener would be a frame no socket ever sees and
//! the client could never tell was missing; installing first and announcing
//! second makes that window empty (`sync-ws-handler.ts:207-236`).

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::ws::WebSocket;
use roost_proto::SyncDomain;
use roost_proto::buffa::Message as _;

use crate::auth::ws_auth_deadline::{REAUTH_CLOSE_CODE, REAUTH_CLOSE_REASON, reauth_expired};
use crate::services::CoordServices;
use crate::sync_ws::admission::EnqueueOutcome;
use crate::sync_ws::commands::ClientContext;
use crate::sync_ws::commands_layout::register_layout_target;
use crate::sync_ws::control_frames::subscribed_frame;
use crate::sync_ws::driver::{Delivery, LinkClose, LinkState, Outbox, SyncLink, now_ms};
use crate::sync_ws::feed::sync_state::mint_socket_id;
use crate::sync_ws::feed::ui::{UiViewer, ui_state_seed_frames};
use crate::sync_ws::live_feed::LiveFeed;
use crate::sync_ws::resource_index::load_sync_resource_index;
use crate::sync_ws::session::SyncV2Session;
use crate::sync_ws::socket::write_frame;
use crate::sync_ws::terminal::snapshot::NoTerminalSnapshotHub;
use crate::sync_ws::upgrade_admission::{
    CONNECTION_REJECTION_CLOSE_CODE, CONNECTION_REJECTION_REASON, SyncScope, VerifiedSyncCaller,
};
use crate::sync_ws::v1_delivery::V1Delivery;
use crate::ui_state::layout_apply::LayoutApplyTargetGuard;
use crate::worker_link::conn_types::CLOSE_REVOKED;

/// A credential whose deadline passed: `4003 reauth required`.
pub(in crate::sync_ws) const REAUTH: LinkClose = LinkClose {
    code: REAUTH_CLOSE_CODE,
    reason: REAUTH_CLOSE_REASON,
};

/// A key revoked between its verification and this socket opening.
const REVOKED: LinkClose = LinkClose {
    code: CLOSE_REVOKED,
    reason: "revoked",
};

/// The barrier could not be written, so no generation was ever announced
/// (`sync-ws-handler.ts:241-246`).
const SUBSCRIBED_SEND_FAILED: LinkClose = LinkClose {
    code: 1011,
    reason: "subscribed send failed",
};

/// The socket's scope or identity could not be established.
const OPEN_FAILED: LinkClose = LinkClose {
    code: 1011,
    reason: "sync open failed",
};

/// No layout-apply target could be registered for this tab
/// (`sync-ws-handler.ts:247-266`).
const CONNECTION_REJECTED: LinkClose = LinkClose {
    code: CONNECTION_REJECTION_CLOSE_CODE,
    reason: CONNECTION_REJECTION_REASON,
};

/// What an open socket holds besides the wire.
pub(in crate::sync_ws) struct OpenedSocket {
    /// The state its listeners and its task share.
    pub link: Arc<SyncLink>,
    /// Its bus subscriptions; dropping them is the unsubscribe.
    pub feed: LiveFeed,
    /// The v2 socket id; empty on a v1 socket.
    pub socket_id: String,
    /// The snapshot-registry handle of a v2 socket.
    registration: Option<u64>,
    /// The layout-apply target; dropping it settles its reserved applies.
    layout_target: Option<LayoutApplyTargetGuard>,
    /// What a layout result is correlated with.
    pub layout_context: ClientContext,
}

/// Open one admitted socket, or name the close it gets instead.
pub(in crate::sync_ws) async fn open_socket(
    socket: &mut WebSocket,
    caller: &VerifiedSyncCaller,
    scope: &SyncScope,
    reauth_at_ms: Option<i64>,
    services: &Arc<CoordServices>,
) -> Result<OpenedSocket, LinkClose> {
    if !services
        .jwt_keys
        .generation_is_current(&caller.fingerprint, caller.key_generation)
    {
        return Err(REVOKED);
    }
    let now = i64::try_from(now_ms()).unwrap_or(i64::MAX);
    if reauth_at_ms.is_some_and(|deadline| reauth_expired(deadline, now)) {
        return Err(REAUTH);
    }
    let index = load_sync_resource_index(&services.db, scope.owner_worker_fp.as_deref())
        .await
        .map_err(|error| {
            tracing::warn!(event = "sync-ws", action = "scope_load_failed", caller_fp = %caller.fingerprint, error = %error);
            OPEN_FAILED
        })?;
    let v2 = scope.domain_generations;
    let socket_id = if v2 {
        mint_socket_id().map_err(|error| {
            tracing::warn!(event = "sync-ws", action = "socket_id_unavailable", error = %error);
            OPEN_FAILED
        })?
    } else {
        String::new()
    };
    let context = ClientContext {
        read_only: scope.read_only,
        tab_id: scope.tab_id.clone(),
        viewer_key: scope.viewer_key.clone(),
        fingerprint: caller.fingerprint.clone(),
        session_ids: BTreeSet::new(),
    };
    let delivery = if v2 {
        let generations = Arc::clone(services.feed.domain_generations());
        Delivery::V2(Box::new(SyncV2Session::new(
            socket_id.clone(),
            generations,
            true,
        )))
    } else {
        Delivery::V1(V1Delivery::new(scope.flow_control))
    };
    let owned_session_ids = index
        .owner_worker_fp
        .is_some()
        .then(|| index.session_ids.clone());
    let link = Arc::new(SyncLink::new(LinkState {
        socket_id: socket_id.clone(),
        caller_fp: caller.fingerprint.clone(),
        delivery,
        context: context.clone(),
        index,
        owned_session_ids,
        replayed_cutoff: scope.since_event_id,
        outbox: Outbox::default(),
        close: None,
        screen: NoTerminalSnapshotHub,
    }));
    let registration = v2.then(|| {
        services
            .feed
            .register_sync_socket(&socket_id, &caller.fingerprint)
    });
    let viewer = match (scope.read_only, v2) {
        (true, _) => UiViewer::suppressed(),
        (false, true) => UiViewer::browser(socket_id.clone()),
        (false, false) => UiViewer {
            browser_ui: true,
            socket_id: None,
        },
    };
    let mut feed = LiveFeed::install(&link, &services.buses, viewer, scope.viewer_key.clone());
    if !v2 {
        // v1 has no domain commands, so its audit source is eager
        // (`sync-feed.ts:204-205`).
        feed.set_audit_subscribed(&link, &services.buses, true);
    }
    let opened = OpenedSocket {
        link,
        feed,
        socket_id,
        registration,
        layout_target: None,
        layout_context: context,
    };
    let opened = if v2 {
        announce_subscribed(socket, opened, services).await?
    } else {
        tracing::warn!(
            event = "sync-ws",
            action = "v1_seed_unported",
            caller_fp = %caller.fingerprint,
            since = scope.since_event_id,
            "the v1 retained seed and durable backfill are not sent yet (sync_ws seed, SY3); only live frames follow"
        );
        opened
    };
    tracing::info!(
        event = "sync-ws",
        action = "open",
        caller_fp = %caller.fingerprint,
        socket_id = %opened.socket_id,
        since = scope.since_event_id,
        sync_v = if v2 { 2 } else { 1 },
        "sync socket open"
    );
    Ok(opened)
}

/// The v2 open after the feed is listening: the `subscribed` barrier, the
/// tab's layout-apply target, the UI state seed, and the recovery refusal.
async fn announce_subscribed(
    socket: &mut WebSocket,
    mut opened: OpenedSocket,
    services: &Arc<CoordServices>,
) -> Result<OpenedSocket, LinkClose> {
    let generations: Vec<(SyncDomain, u64, bool)> = match &opened.link.lock().delivery {
        Delivery::V2(session) => session
            .domains
            .iter()
            .map(|domain| (domain.domain, domain.generation, domain.subscribed))
            .collect(),
        Delivery::V1(_) => Vec::new(),
    };
    let barrier = subscribed_frame(
        &opened.socket_id,
        services.feed.process_epoch(),
        &generations,
    );
    if write_frame(socket, barrier.encode_to_vec()).await.is_err() {
        release_socket(opened, services);
        return Err(SUBSCRIBED_SEND_FAILED);
    }
    match register_layout_target(
        &services.ui_state,
        &opened.layout_context,
        &opened.socket_id,
    ) {
        Ok(target) => opened.layout_target = target,
        Err(_) => {
            release_socket(opened, services);
            return Err(CONNECTION_REJECTED);
        }
    }
    if !opened.layout_context.read_only {
        for frame in ui_state_seed_frames(services.ui_state.states()) {
            opened.link.deliver_with(|_| Some(frame));
        }
    }
    refuse_recovery(&opened.link);
    Ok(opened)
}

/// A v2 socket that asked to resume from `since` cannot be replayed yet, so
/// its terminal domain is reset with v2's recovery-failure reason
/// (`sync-feed.ts:340-346`): the client re-hydrates from the authoritative
/// list instead of believing the gap closed.
fn refuse_recovery(link: &SyncLink) {
    let mut state = link.lock();
    let since = state.replayed_cutoff;
    if since == 0 {
        return;
    }
    tracing::warn!(
        event = "sync-ws",
        action = "recovery_unported",
        socket_id = %state.socket_id,
        since,
        "durable recovery above `since` is not replayed yet (sync_ws seed, SY3); the terminal domain is reset"
    );
    let Delivery::V2(session) = &mut state.delivery else {
        return;
    };
    if let EnqueueOutcome::Reset(notice) =
        session.reset_domain(SyncDomain::Terminal, "recovery_failed")
    {
        state.send_control(&notice.to_frame(), now_ms());
    }
}

/// Release everything an open socket holds, in v2's `cleanupSocket` order:
/// listeners first, so nothing more can reach the link, then the queues, the
/// snapshot binding, the terminal views and the layout target.
pub(in crate::sync_ws) fn release_socket(opened: OpenedSocket, services: &Arc<CoordServices>) {
    let OpenedSocket {
        link,
        feed,
        socket_id,
        registration,
        layout_target,
        ..
    } = opened;
    drop(feed);
    link.lock().retire();
    if let Some(registration) = registration {
        services
            .feed
            .unregister_sync_socket(&socket_id, registration);
        services.views.close_socket(&socket_id, now_ms());
    }
    drop(layout_target);
}
