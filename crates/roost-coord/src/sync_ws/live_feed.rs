//! The live half of one socket's Sync feed: every bus subscribed, each message
//! narrowed to the resources this socket may observe, turned into a frame by
//! its adapter in `sync_ws::feed`, and delivered into the socket's link.
//!
//! Installed by `sync_ws::socket` before the `subscribed` barrier escapes, and
//! dropped at teardown, which is the unsubscribe. Ports the listener engine of
//! `apps/coord/src/sync/sync-feed.ts` (`startSyncFeed`'s `unsubs` list and the
//! lazy audit source). Which live session events a recovery holds or drops is
//! `sync_ws::session_replay`; the durable rows and retained seeds themselves
//! are `sync_ws::backfill` and `sync_ws::seed`.
//!
//! SIXTEEN eager subscriptions, one per bus in `Buses` except `audit_bus`,
//! which is on demand -- eagerly for a v1 socket, which has no domain
//! commands, and only on `domainSubscribe` for a v2 one.
use std::any::Any;
use std::sync::Arc;

use roost_protocol::wire::{SessionEvent, WorkerPresenceEvent, WorkspaceDelta};

use crate::events::bus::Subscription;
use crate::events::bus_domains::Buses;
use crate::events::bus_messages::{AuditRow, SessionBusMessage};
use crate::sync_ws::backfill::reset_terminal_for_recovery;
use crate::sync_ws::driver::{LinkState, SyncLink};
use crate::sync_ws::feed::FeedFrame;
use crate::sync_ws::feed::frames::{
    agent_status_frame, audit_frame, clipboard_history_frame, mcp_frame, pair_frame,
    session_bell_frame, session_clipboard_frame, session_command_finished_frame,
    session_message_frame, session_title_frame, task_frame, workspace_frame,
};
use crate::sync_ws::feed::last_activity::last_activity_frame;
use crate::sync_ws::feed::presence::{presence_echo_is_own_notice, session_presence_frame};
use crate::sync_ws::feed::ui::{UiViewer, ui_bus_frame};
use crate::sync_ws::feed::worker_frames::{worker_presence_frame, worker_routable_frame};
use crate::sync_ws::session_replay::LiveVerdict;

/// One socket's live subscriptions. Dropping it unsubscribes every bus.
pub struct LiveFeed {
    /// The sixteen eager subscriptions, held only to be dropped.
    subscriptions: Vec<Box<dyn Any + Send + Sync>>,
    /// The audit source, present while this socket wants audit rows.
    audit: Option<Subscription<AuditRow>>,
}

impl std::fmt::Debug for LiveFeed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LiveFeed")
            .field("subscriptions", &self.subscriptions.len())
            .field("audit", &self.audit.is_some())
            .finish()
    }
}

impl LiveFeed {
    /// Subscribe every eager bus for this socket.
    ///
    /// `viewer` is the UI stream's gate and `viewer_key` the presence echo
    /// filter's; both are fixed for the socket's lifetime.
    pub fn install(
        link: &Arc<SyncLink>,
        buses: &Buses,
        viewer: UiViewer,
        viewer_key: Option<String>,
    ) -> Self {
        let mut subscriptions: Vec<Box<dyn Any + Send + Sync>> = Vec::with_capacity(16);
        let sink = Arc::clone(link);
        subscriptions.push(Box::new(buses.session_bus.subscribe(move |message| {
            sink.deliver_with(|state| route_session(state, message));
        })));
        let sink = Arc::clone(link);
        subscriptions.push(Box::new(buses.presence_bus.subscribe(move |event| {
            sink.deliver_with(|state| route_worker_presence(state, event));
        })));
        let sink = Arc::clone(link);
        subscriptions.push(Box::new(buses.workspace_bus.subscribe(move |delta| {
            sink.deliver_with(|state| route_workspace(state, delta));
        })));
        let sink = Arc::clone(link);
        subscriptions.push(Box::new(buses.task_bus.subscribe(move |message| {
            sink.deliver_with(|state| state.index.is_install_wide().then(|| task_frame(message)));
        })));
        let sink = Arc::clone(link);
        subscriptions.push(Box::new(buses.mcp_bus.subscribe(move |message| {
            sink.deliver_with(|state| {
                if !state.index.is_install_wide() {
                    return None;
                }
                refused_as_none("mcp_bus", mcp_frame(message))
            });
        })));
        let sink = Arc::clone(link);
        subscriptions.push(Box::new(buses.pair_bus.subscribe(move |delta| {
            sink.deliver_with(|state| state.index.is_install_wide().then(|| pair_frame(delta)));
        })));
        let sink = Arc::clone(link);
        subscriptions.push(Box::new(buses.global_presence_bus.subscribe(
            move |update| {
                sink.deliver_with(|state| {
                    let observed = state.index.session_ids.contains(&update.session_id);
                    let own_echo = presence_echo_is_own_notice(&update.data, viewer_key.as_deref());
                    (observed && !own_echo).then(|| session_presence_frame(update))
                });
            },
        )));
        let sink = Arc::clone(link);
        subscriptions.push(Box::new(buses.title_bus.subscribe(move |title| {
            sink.deliver_with(|state| {
                observes(state, &title.session_id).then(|| session_title_frame(title))
            });
        })));
        let sink = Arc::clone(link);
        subscriptions.push(Box::new(buses.clipboard_bus.subscribe(move |write| {
            sink.deliver_with(|state| {
                observes(state, &write.session_id).then(|| session_clipboard_frame(write))
            });
        })));
        let sink = Arc::clone(link);
        subscriptions.push(Box::new(buses.clipboard_history_bus.subscribe(
            move |change| {
                sink.deliver_with(|state| {
                    state
                        .index
                        .is_install_wide()
                        .then(|| clipboard_history_frame(change))
                });
            },
        )));
        let sink = Arc::clone(link);
        subscriptions.push(Box::new(buses.command_finished_bus.subscribe(
            move |finished| {
                sink.deliver_with(|state| {
                    observes(state, &finished.session_id)
                        .then(|| session_command_finished_frame(finished))
                });
            },
        )));
        let sink = Arc::clone(link);
        subscriptions.push(Box::new(buses.bell_bus.subscribe(move |bell| {
            sink.deliver_with(|state| {
                observes(state, &bell.session_id).then(|| session_bell_frame(bell))
            });
        })));
        let sink = Arc::clone(link);
        subscriptions.push(Box::new(buses.last_activity_bus.subscribe(move |update| {
            sink.deliver_with(|state| {
                observes(state, &update.session_id).then(|| last_activity_frame(update))
            });
        })));
        let sink = Arc::clone(link);
        subscriptions.push(Box::new(buses.worker_routable_bus.subscribe(
            move |routable| {
                sink.deliver_with(|state| {
                    Some(worker_routable_frame(routable, &state.index.worker_fps))
                });
            },
        )));
        let sink = Arc::clone(link);
        subscriptions.push(Box::new(buses.agent_status_bus.subscribe(move |status| {
            sink.deliver_with(|state| {
                observes(state, status.common.session_id.as_str())
                    .then(|| agent_status_frame(status))
            });
        })));
        let sink = Arc::clone(link);
        subscriptions.push(Box::new(buses.ui_bus.subscribe(move |message| {
            sink.deliver_with(|_| ui_bus_frame(message, &viewer));
        })));
        Self {
            subscriptions,
            audit: None,
        }
    }

    /// Start or stop the audit source. Only an install-wide viewer is ever
    /// subscribed: a worker socket's audit domain may be toggled, but the rows
    /// are install-wide and it is not one of their viewers
    /// (`sync-feed.ts:188-203`).
    pub fn set_audit_subscribed(&mut self, link: &Arc<SyncLink>, buses: &Buses, subscribed: bool) {
        if !subscribed {
            self.audit = None;
            return;
        }
        if self.audit.is_some() || !link.lock().index.is_install_wide() {
            return;
        }
        let sink = Arc::clone(link);
        self.audit = Some(buses.audit_bus.subscribe(move |row| {
            sink.deliver_with(|_| Some(audit_frame(row)));
        }));
    }
}

/// Whether this socket observes `session_id`'s session-keyed frames.
fn observes(state: &LinkState, session_id: &str) -> bool {
    state.index.session_ids.contains(session_id)
}

/// A durable session event: keep the scope current, then admit the frame.
///
/// The scope moves FIRST and whether or not the frame is admitted, so the
/// title or presence that follows an `opened` finds its session already in
/// scope (`sync-feed.ts:210-222`).
fn route_session(state: &mut LinkState, message: &SessionBusMessage) -> Option<FeedFrame> {
    let index = &mut state.index;
    match &message.event {
        SessionEvent::Snapshot {
            worker_fp,
            sessions,
            ..
        } if index.owns_worker(worker_fp.as_str()) => {
            index.session_ids.extend(
                sessions
                    .iter()
                    .map(|session| session.id.as_str().to_owned()),
            );
        }
        SessionEvent::Opened {
            worker_fp,
            session_id,
            ..
        } if index.owns_worker(worker_fp.as_str()) => {
            index.session_ids.insert(session_id.as_str().to_owned());
        }
        SessionEvent::Closed { session_id, .. } => {
            index.session_ids.remove(session_id.as_str());
        }
        _ => {}
    }
    match state.replay.admit_live(message) {
        LiveVerdict::Emit => emit_session_frame(state, message),
        LiveVerdict::Duplicate | LiveVerdict::Held => None,
        LiveVerdict::Abort { reason, emit, held } => {
            reset_terminal_for_recovery(state, reason);
            // The events the abandoned recovery was holding, in id order and
            // BEFORE the one that ended it, because they are older and the
            // client folds in arrival order. This is the difference between a
            // browser whose session opened and one that never hears about it.
            let mut last = None;
            for released in held {
                last = emit_session_frame(state, &released).or(last);
            }
            if emit {
                last = emit_session_frame(state, message).or(last);
            }
            last
        }
    }
}

/// One durable session event as this socket's frame, if the socket carries it
/// at all (`sync-feed.ts:119-122`, `emitSessionFrame`). The live feed and the
/// backfill both deliver through here, so a worker socket's narrowing is the
/// same for a replayed row as for a live one.
pub(in crate::sync_ws) fn emit_session_frame(
    state: &mut LinkState,
    message: &SessionBusMessage,
) -> Option<FeedFrame> {
    if !admit_owned_session_event(state, &message.event) {
        return None;
    }
    refused_as_none("session_bus", session_message_frame(message))
}

/// A worker socket carries only its own sessions, and keeps knowing them after
/// they close so a late `closed` still reaches it (`sync-feed.ts:96-114`).
fn admit_owned_session_event(state: &mut LinkState, event: &SessionEvent) -> bool {
    let owner = state.index.owner_worker_fp.as_deref();
    let Some(owned) = state.owned_session_ids.as_mut() else {
        return true;
    };
    match event {
        SessionEvent::Snapshot {
            worker_fp,
            sessions,
            ..
        } => {
            if Some(worker_fp.as_str()) != owner {
                return false;
            }
            owned.extend(
                sessions
                    .iter()
                    .map(|session| session.id.as_str().to_owned()),
            );
            true
        }
        SessionEvent::Opened {
            worker_fp,
            session_id,
            ..
        } => {
            if Some(worker_fp.as_str()) != owner {
                return false;
            }
            owned.insert(session_id.as_str().to_owned());
            true
        }
        other => other
            .session_id()
            .is_some_and(|session_id| owned.contains(session_id.as_str())),
    }
}

/// A worker registry event, narrowed to the workers this socket observes.
fn route_worker_presence(state: &mut LinkState, event: &WorkerPresenceEvent) -> Option<FeedFrame> {
    let worker_fp = match event {
        WorkerPresenceEvent::Registered { worker } => &worker.fp,
        WorkerPresenceEvent::Heartbeat { fp, .. } | WorkerPresenceEvent::Removed { fp } => fp,
    };
    if !state.index.owns_worker(worker_fp.as_str()) {
        return None;
    }
    match event {
        WorkerPresenceEvent::Registered { .. } => {
            state.index.worker_fps.insert(worker_fp.clone());
        }
        WorkerPresenceEvent::Removed { .. } => {
            state.index.worker_fps.remove(worker_fp);
        }
        WorkerPresenceEvent::Heartbeat { .. } => {}
    }
    refused_as_none("presence_bus", worker_presence_frame(event))
}

/// A workspace change, narrowed to the workspaces this socket observes
/// (`sync-feed.ts:233-245`).
fn route_workspace(state: &mut LinkState, delta: &WorkspaceDelta) -> Option<FeedFrame> {
    let index = &mut state.index;
    let owned = match delta {
        WorkspaceDelta::Created { workspace } | WorkspaceDelta::Updated { workspace } => {
            index.owns_worker(workspace.worker_fp.as_str())
        }
        WorkspaceDelta::Deleted { id } | WorkspaceDelta::SessionsSet { id, .. } => {
            index.is_install_wide() || index.workspace_ids.contains(id.as_str())
        }
    };
    match delta {
        WorkspaceDelta::Deleted { id } => {
            index.workspace_ids.remove(id.as_str());
        }
        WorkspaceDelta::Created { workspace } | WorkspaceDelta::Updated { workspace } if owned => {
            index.workspace_ids.insert(workspace.id.as_str().to_owned());
        }
        _ => {}
    }
    owned.then(|| workspace_frame(delta))
}

/// An adapter's refusal, logged and dropped: one unencodable message must not
/// cost the socket the rest of its feed.
fn refused_as_none(
    bus: &'static str,
    frame: Result<FeedFrame, crate::sync_ws::feed::FeedRefusal>,
) -> Option<FeedFrame> {
    frame
        .inspect_err(|refusal| {
            tracing::debug!(event = "sync-ws", action = "feed_frame_refused", bus, reason = %refusal);
        })
        .ok()
}
