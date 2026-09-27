//! Toasts: the notification cards, their identity, and their deadlines.
//!
//! Three rules live here rather than at the call sites, because each of them is
//! one a call site can forget:
//!
//! 1. IDENTITY IS DERIVED FROM THE EVENT. A [`ToastId`] names the source event
//!    — a Sync frame with its delivery sequence, a Connect answer with its call
//!    id, a spawn settlement with its attempt — and the thing it is about. It
//!    is never a counter, so the same event cannot produce two live cards: the
//!    stack holds AT MOST ONE toast per id, and re-adding an id replaces it in
//!    place. A redelivered frame updates a card instead of stacking a second
//!    one, and two genuinely different frames still get two cards.
//! 2. EVERY TOAST DISMISSES. A toast with a ttl carries an ABSOLUTE deadline
//!    and [`expire_due_toasts`] removes it. There is no timer in this crate, so
//!    the sweep is the only thing that can retire a card, which is why the
//!    deadline is data rather than a closure a host may drop.
//! 3. A HELD CARD DOES NOT BURN ITS WINDOW. [`hold_toast_dismiss`] freezes the
//!    deadline and [`release_toast_dismiss`] resumes it with the time that was
//!    left, so reading a long error never races the clock that removes it.
//!
//! Ported from `apps/web/src/store/toastStore.ts`; the deviations are the
//! event-derived identity, the data deadline in place of `setTimeout`, and
//! [`take_toast_action`] making "dismiss, then reveal" the only possible order
//! for a card that offers one.

pub mod identity;

use std::collections::BTreeMap;

use crate::store::Store;
use crate::store::toasts::identity::{Toast, ToastAction, ToastId, ToastKind, ToastOptions};

/// The live cards, in the order they were first raised.
///
/// A `BTreeMap` keyed by id, not a `Vec`: the whole point of the identity is
/// that a repeat is impossible, and a keyed map makes that a type-level fact
/// rather than a filter a caller has to remember. The order is the id order,
/// which is stable across a redelivery, so a replaced card keeps its place
/// instead of jumping to the end.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ToastStack {
    toasts: BTreeMap<ToastId, Toast>,
}

impl ToastStack {
    /// No cards.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many cards are live.
    pub fn len(&self) -> usize {
        self.toasts.len()
    }

    /// Whether no card is live.
    pub fn is_empty(&self) -> bool {
        self.toasts.is_empty()
    }

    /// The live cards.
    pub fn toasts(&self) -> impl Iterator<Item = &Toast> {
        self.toasts.values()
    }

    /// One card by id.
    pub fn toast(&self, id: &ToastId) -> Option<&Toast> {
        self.toasts.get(id)
    }

    /// Whether a card with this id is live. The check a caller wants before
    /// composing new text for it.
    pub fn contains(&self, id: &ToastId) -> bool {
        self.toasts.contains_key(id)
    }
}

/// Raise a card, or replace the one this exact event already raised.
///
/// Returns the card's id. The replace is the dedup: the same frame arriving twice
/// updates one card's text and window, and does not append.
///
/// `now_ms` is the host's single reading of the clock, the same one
/// `handle_event` takes, so a card's window cannot disagree with the deadline
/// that retires it.
pub fn add_toast(
    store: &mut Store,
    id: ToastId,
    msg: impl Into<String>,
    kind: ToastKind,
    options: ToastOptions,
    now_ms: u64,
) -> ToastId {
    let id = raise_toast(&mut store.toasts, id, msg, kind, options, now_ms);
    store.note_change();
    id
}

/// The one place a card enters the map, WITHOUT bumping `revision`.
///
/// The public mutations here bump, because a host that watches the counter for a
/// repaint cannot see a card that arrived without it. This inner function exists
/// for a mutation that is COMPOSITE — a spawn settlement that both drops a
/// placeholder and raises a card is one user-visible event with two writes, and
/// two bumps would be two repaints of one change. Its only callers are the
/// public mutations in this module, each of which bumps exactly once.
pub fn raise_toast(
    toasts: &mut ToastStack,
    id: ToastId,
    msg: impl Into<String>,
    kind: ToastKind,
    options: ToastOptions,
    now_ms: u64,
) -> ToastId {
    let ttl_ms = options.ttl_ms.unwrap_or(kind.default_ttl_ms());
    let previous = toasts.toasts.get(&id);
    let mut toast = Toast {
        id: id.clone(),
        msg: msg.into(),
        kind,
        details: options.details,
        action: options.action,
        target_session_id: options.target_session_id,
        expires_at_ms: ttl_ms.map(|ttl| now_ms.saturating_add(ttl)),
        // A replace keeps the window the card already had when the caller did
        // not ask for a new one: a redelivered frame refreshing a card's text
        // must not hand a user who is reading it a fresh eight seconds.
        remaining_ms: ttl_ms.or(previous.map_or(0, |prior| prior.remaining_ms)),
        held: previous.is_some_and(|prior| prior.held),
    };
    if let Some(sticky) = previous.filter(|prior| prior.held) {
        toast.expires_at_ms = sticky.expires_at_ms;
    }
    toasts.toasts.insert(id.clone(), toast);
    tracing::debug!(
        target: "store",
        kind = kind.as_str(),
        subject = %id.subject,
        "toast raised"
    );
    id
}

/// Remove one card, by hand or because a deadline passed.
pub fn dismiss_toast(store: &mut Store, id: &ToastId) -> bool {
    if store.toasts.toasts.remove(id).is_none() {
        return false;
    }
    store.note_change();
    true
}

/// Take a card's button and remove the card in the same step.
///
/// The order is the rule, not an accident: `AgentNotificationBridge.tsx:113`
/// dismisses before it navigates, and a host that revealed first would leave a
/// card on screen pointing at the session the user just left. `None` means
/// there was no such card, or it offered no button.
pub fn take_toast_action(store: &mut Store, id: &ToastId) -> Option<ToastAction> {
    let toast = store.toasts.toasts.remove(id)?;
    store.note_change();
    toast.action
}

/// Remove every card whose deadline has passed.
///
/// Called by the sweep, which is the only path by which time reaches this
/// crate. Returns whether anything was removed, so a caller can skip a repaint
/// it has already accounted for.
pub fn expire_due_toasts(store: &mut Store, now_ms: u64) -> bool {
    let due: Vec<ToastId> = store
        .toasts
        .toasts
        .values()
        .filter(|toast| toast.is_due(now_ms))
        .map(|toast| toast.id.clone())
        .collect();
    if due.is_empty() {
        return false;
    }
    for id in &due {
        store.toasts.toasts.remove(id);
    }
    store.note_change();
    true
}

/// Freeze a card's window, because the pointer or focus is resting on it.
///
/// It is the only place the details text and the highlighted target can be
/// read. A card with no window cannot be held, and neither can one that is
/// already held.
pub fn hold_toast_dismiss(store: &mut Store, id: &ToastId, now_ms: u64) -> bool {
    let Some(toast) = store.toasts.toasts.get_mut(id) else {
        return false;
    };
    if toast.held || toast.expires_at_ms.is_none() {
        return false;
    }
    let deadline = toast.expires_at_ms.unwrap_or(now_ms);
    toast.remaining_ms = deadline.saturating_sub(now_ms);
    toast.expires_at_ms = None;
    toast.held = true;
    // The card's countdown is what renders, so freezing it is a change the host
    // has to repaint.
    store.note_change();
    true
}

/// Resume a held card's window with the time that was left.
///
/// A card released with nothing left is removed rather than re-armed, so
/// releasing at the deadline does not resurrect a card the user was done with.
pub fn release_toast_dismiss(store: &mut Store, id: &ToastId, now_ms: u64) -> bool {
    let remaining = {
        let Some(toast) = store.toasts.toasts.get(id) else {
            return false;
        };
        if !toast.held {
            return false;
        }
        toast.remaining_ms
    };
    if remaining == 0 {
        return dismiss_toast(store, id);
    }
    if let Some(toast) = store.toasts.toasts.get_mut(id) {
        toast.held = false;
        toast.expires_at_ms = Some(now_ms.saturating_add(remaining));
    }
    // The countdown resumes, which is as much a change to what renders as
    // freezing it was.
    store.note_change();
    true
}

/// Drop every card about `session_id`, and report whether there were any.
///
/// A card whose button reveals a session that no longer exists is a button that
/// navigates to nothing, so a session's cards go with it. The match is on the
/// card's OWN subject or its target, per key: a card about a different session is
/// left alone, because emptying the whole stack would take every unrelated
/// message with it.
pub fn drop_toasts_for_session(store: &mut Store, session_id: &str) -> bool {
    let dropped = remove_toasts_for_session(&mut store.toasts, session_id);
    if dropped {
        store.note_change();
    }
    dropped
}

/// The removal itself, WITHOUT bumping `revision`, for a teardown that is one
/// larger mutation — `Store::forget_session`, whose caller already notes the
/// change once for the replica, the find results and the input lane it drops with
/// the cards. See [`raise_toast`] for why the inner form exists.
pub fn remove_toasts_for_session(toasts: &mut ToastStack, session_id: &str) -> bool {
    let doomed: Vec<ToastId> = toasts
        .toasts
        .iter()
        .filter(|(id, toast)| {
            id.names_session(session_id) || toast.target_session_id.as_deref() == Some(session_id)
        })
        .map(|(id, _)| id.clone())
        .collect();
    for id in &doomed {
        toasts.toasts.remove(id);
    }
    !doomed.is_empty()
}

/// Drop every card, at a credential boundary.
///
/// A card from the previous credential names a session and a path the new one
/// cannot see, and the text of a failure can quote a machine name.
pub fn clear_toasts_for_account_boundary(store: &mut Store) {
    if clear_all(&mut store.toasts) {
        store.note_change();
    }
}

/// The clear itself, WITHOUT bumping `revision`, for a boundary that is clearing
/// several slices as one mutation. See [`raise_toast`] for why the inner form
/// exists.
pub fn clear_all(toasts: &mut ToastStack) -> bool {
    let had_any = !toasts.toasts.is_empty();
    toasts.toasts.clear();
    had_any
}
