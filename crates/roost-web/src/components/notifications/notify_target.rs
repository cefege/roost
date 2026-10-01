//! The session a hovered notification points at, shared with the surfaces that
//! paint the ring: a pane tab when one represents the target, and the sidebar
//! folder row when none does. Ports `apps/web/src/store/notifyTarget.ts` and the
//! ring rule in `assets/components/notifications/NotificationDock.css`.
//!
//! It is a Dioxus signal rather than store state because it is a pointer
//! affordance with no lifetime of its own: it is set by a card's hover and
//! cleared by the same card's leave, and nothing else may write it. Putting it
//! in the store would make a hover look like durable client state.
//!
//! A SURFACE READS THE HOLD THROUGH `hold`, WHICH SUBSCRIBES. A hover arrives as
//! a `mouseenter` handler and a handler schedules no repaint of its own, so a row
//! that peeked at the hold kept the frame it first drew — the pointer was on the
//! card and nothing on the page said so.

use dioxus::prelude::*;
use roost_client_core::Store;
use roost_client_core::store::WorkerPaths;
use roost_client_core::store::selectors::{
    live_session_ids_for_folder, session_by_id, session_folder_key,
};
use roost_client_core::store::toasts::ToastId;

use crate::route_session::active_session_for_path;

/// Who is ringing, and which toast is responsible for it.
///
/// The identity is not decoration. Two cards can be hovered in turn, and the
/// later one to take the ring is not necessarily the later one to leave it: a
/// pointer that jumps straight from one card to the next makes the first card
/// emit its leave after the second has already rung, and a plain `clear` there
/// takes the second card's ring down with it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RingHold {
    /// The card that took the ring.
    pub holder: Option<ToastId>,
    /// The session it names.
    pub session_id: Option<String>,
}

/// The ring's current target, in context for the whole authorized shell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NotifyTarget(Signal<RingHold>);

impl NotifyTarget {
    /// Provide the ring in the calling scope.
    pub fn provide() -> Self {
        use_context_provider(|| Self(Signal::new(RingHold::default())))
    }

    /// Ring `session_id` for `toast`, or clear the ring for a subject-less
    /// card. Naming the holder is what lets that card's own release undo it.
    pub fn ring(&mut self, toast: &ToastId, session_id: Option<&str>) {
        self.0.set(RingHold {
            holder: Some(toast.clone()),
            session_id: session_id.map(str::to_owned),
        });
    }

    /// Drop the ring, if `toast` is the one holding it.
    ///
    /// The current value is the question here and a subscription is not wanted:
    /// this runs in a pointer handler, where the scope that must repaint is
    /// somebody else's.
    pub fn clear(&mut self, toast: &ToastId) {
        if self.0.peek().holder.as_ref() == Some(toast) {
            self.0.set(RingHold::default());
        }
    }

    /// The session currently ringed, if any.
    pub fn session_id(&self) -> Option<String> {
        self.0().session_id.clone()
    }

    /// The raw signal, for a surface that paints the ring from a read.
    pub fn signal(&self) -> Signal<RingHold> {
        self.0
    }

    /// The hold, read so that the calling render re-runs when it moves.
    pub fn hold(&self) -> RingHold {
        self.0()
    }

    /// Whether a surface should paint the ring for `session_id`.
    pub fn rings(&self, session_id: &str) -> bool {
        self.hold().session_id.as_deref() == Some(session_id)
    }
}

/// The ring from the calling scope, or `None` where the authorized shell never
/// mounted.
///
/// A row that can render outside the shell — a native render harness, a
/// component preview — paints no ring rather than panicking on a missing
/// context, which would take the whole render down with it.
pub fn use_notify_target() -> Option<NotifyTarget> {
    try_use_context::<NotifyTarget>()
}

/// The `data-notify-target` attribute a ringing surface carries.
pub fn ring_attribute(target: &NotifyTarget, session_id: &str) -> Option<&'static str> {
    target.rings(session_id).then_some("true")
}

/// Every session the deck's on-screen tab strip already shows, for `path`.
///
/// The deck paints ONE folder's arrangement, so this is the whole set a tab ring
/// can land on. `reconcile` gives every live session of that folder a tab, which
/// is what makes the folder's live set the tab set.
#[must_use]
pub fn open_tab_session_ids(store: &Store, paths: &dyn WorkerPaths, path: &str) -> Vec<String> {
    active_session_for_path(store, paths, path).map_or_else(Vec::new, |session| {
        let folder_key = session_folder_key(store, paths, session);
        live_session_ids_for_folder(store, paths, &folder_key)
    })
}

/// The folder key whose sidebar row carries the ring, or `None` when a pane tab
/// already answers for the target.
///
/// One agent rings once. A target with a tab on screen is answered by that tab,
/// and ringing its folder row as well would put two rings on one session, on
/// two surfaces the reader cannot act on together.
#[must_use]
pub fn ringing_folder_key(
    hold: &RingHold,
    store: &Store,
    paths: &dyn WorkerPaths,
    tab_session_ids: &[String],
) -> Option<String> {
    let session_id = hold.session_id.as_deref()?;
    if tab_session_ids.iter().any(|open| open == session_id) {
        return None;
    }
    let session = session_by_id(store, session_id)?;
    Some(session_folder_key(store, paths, session))
}

/// The `data-notify-target` a folder row carries.
#[must_use]
pub fn folder_ring_attribute(
    hold: &RingHold,
    store: &Store,
    paths: &dyn WorkerPaths,
    tab_session_ids: &[String],
    folder_key: &str,
) -> Option<&'static str> {
    ringing_folder_key(hold, store, paths, tab_session_ids)
        .filter(|ringing| ringing == folder_key)
        .map(|_| "true")
}
