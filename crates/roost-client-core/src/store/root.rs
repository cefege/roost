//! The credential boundary: what an account's data is, and what survives the
//! account going away.
//!
//! v2's `root.ts` held every record in one Solid store and answered the boundary
//! question by naming the slices to clear. Here the wire-shaped records belong to
//! their own modules, so this file answers the same question by naming the
//! MUTATIONS — one function per slice, each of which bumps `revision` and each of
//! which is the only way that slice is emptied. "What does a sign-out clear?" then
//! has one answer, written once, instead of one answer per module that has to be
//! kept in agreement.
//!
//! The rule the boundary exists for: an in-flight result from the PREVIOUS
//! credential must not be able to land in the new one. `auth_generation` is the
//! token for that — it advances at the boundary, and every piece of
//! credential-bound work captures it and asks [`captured_generation_is_current`]
//! before it writes. `command-palette-data.ts:235-237` does this comparison by
//! hand at two call sites; here it is one predicate on the store.
//!
//! Ported from `apps/web/src/store/root.ts`; the deviations are the mutation list
//! in place of a slice list, and the generation moving to the same place the
//! comparison is asked from.

use crate::store::Store;

/// Whether this browser's device key is trusted by the coordinator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BrowserAccessState {
    /// `checking` until the protected sessions snapshot publishes. Every
    /// protected surface is gated on this, so the default is the closed one.
    #[default]
    Checking,
    /// The coordinator published the protected sessions snapshot.
    Authorized,
    /// The coordinator refused this device key.
    Unauthorized,
}

impl BrowserAccessState {
    /// The wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Checking => "checking",
            Self::Authorized => "authorized",
            Self::Unauthorized => "unauthorized",
        }
    }
}

/// Advance the generation, invalidating every token captured before now.
///
/// The ONLY way the generation moves. A caller that wants its work to be
/// abandoned increments here rather than minting a generation of its own, because
/// two counters cannot be compared against each other.
pub fn invalidate_auth_resources(store: &mut Store) -> u64 {
    let generation = store.auth_generation;
    store.auth_generation = generation.saturating_add(1);
    store.note_change();
    tracing::info!(target: "store", generation, "auth generation advanced");
    generation
}

/// Whether work that captured `captured` may still write.
///
/// The answer a command-palette action, a pairing ceremony, and a
/// `doClose`/`doNewTab` continuation ask before acting. Passing is not enough
/// to be current, and being current is not a promise — it is the only question
/// this predicate answers.
pub fn captured_generation_is_current(store: &Store, captured: u64) -> bool {
    store.auth_generation == captured
}

/// Record whether this device key is trusted.
///
/// `store/browser-access.ts` owns WHEN a device becomes authorized or is refused;
/// this owns the value, because the root shell gates every protected surface on
/// it and a second copy of the answer would be a second gate.
pub fn set_browser_access_state(store: &mut Store, state: BrowserAccessState) -> bool {
    if store.browser_access_state == state {
        return false;
    }
    store.browser_access_state = state;
    store.note_change();
    tracing::info!(target: "store", state = state.as_str(), "browser access");
    true
}

/// Clear every record an authenticated list or snapshot populated.
///
/// The credential-bound records of the plumbing slices go here, each through its
/// own non-bumping inner clear so its own bookkeeping — dismissal deadlines, rate
/// samples, tombstones — goes with it. The wire-shaped records (sessions,
/// replicas, find results, the recovery cursor) are `CredentialsDiscarded`'s to
/// clear (`handle_event.rs:126-135`), because the credential that owned them is
/// the credential that was revoked.
///
/// ONE `revision` for the whole clear: it is one user-visible event, and a repaint
/// per emptied slice is a repaint storm on a large account.
pub fn clear_auth_scoped_state(store: &mut Store) {
    let had_any = !store.mcp_relays.is_empty()
        || !store.pair_requests.is_empty()
        || crate::store::toasts::clear_all(&mut store.toasts)
        || crate::store::transfers::clear_all(&mut store.transfers)
        || !store.spawns.is_empty()
        || !store.pending_closes.is_empty();
    if !had_any {
        return;
    }
    store.mcp_relays.clear();
    store.pair_requests.clear();
    store.spawns.reset();
    store.pending_closes.clear();
    store.note_change();
    tracing::info!(target: "store", "auth-scoped records cleared");
}

/// Everything a sign-out owes, in ONE mutation and therefore ONE `revision`.
///
/// The replicas and the recovery cursor are `CredentialsDiscarded`'s; this is the
/// rest. The generation advances so no work captured under the old credential can
/// land, the access state returns to `checking` because the new credential has not
/// been checked, and every credential-bound record goes. Three writes, one event,
/// one repaint.
pub fn clear_account_state_for_logout(store: &mut Store) {
    let had_scoped = !store.mcp_relays.is_empty()
        || !store.pair_requests.is_empty()
        || crate::store::toasts::clear_all(&mut store.toasts)
        || crate::store::transfers::clear_all(&mut store.transfers)
        || !store.spawns.is_empty()
        || !store.pending_closes.is_empty();
    store.mcp_relays.clear();
    store.pair_requests.clear();
    store.spawns.reset();
    store.pending_closes.clear();
    store.workers.clear();
    store.auth_generation = store.auth_generation.saturating_add(1);
    let access_was_set = store.browser_access_state != BrowserAccessState::Checking;
    store.browser_access_state = BrowserAccessState::Checking;
    if had_scoped || access_was_set {
        store.note_change();
    }
    tracing::info!(target: "store", "account state cleared for logout");
}
