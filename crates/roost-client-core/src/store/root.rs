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
//! Ported from `apps/web/src/store/root.ts` and `apps/web/src/store/browser-access.ts`; the deviations are the mutation list
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
    // The clears run FIRST and the predicate is read off their return values.
    // Written as one `||` chain they are short-circuited: the first non-empty
    // collection answers `true` and every later `clear_all` is never called,
    // so a card naming a machine outlives the credential that raised it. The
    // disjunction is unchanged, so this changes WHICH slices get emptied, not
    // whether the caller sees one `revision`.
    let toasts_cleared = crate::store::toasts::clear_all(&mut store.toasts);
    let transfers_cleared = crate::store::transfers::clear_all(&mut store.transfers);
    let had_any = !store.mcp_relays.is_empty()
        || !store.pair_requests.is_empty()
        || toasts_cleared
        || transfers_cleared
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
    // Same shape and same hazard as `clear_auth_scoped_state`, and the same
    // reason the clears come first: `||` short-circuits, and a `pair_request`
    // made `true` before the toast and transfer clears were ever called.
    let toasts_cleared = crate::store::toasts::clear_all(&mut store.toasts);
    let transfers_cleared = crate::store::transfers::clear_all(&mut store.transfers);
    let had_scoped = !store.mcp_relays.is_empty()
        || !store.pair_requests.is_empty()
        || toasts_cleared
        || transfers_cleared
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

/// The coordinator's build and public URL (v2 `rootStore.coord_identity`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordIdentity {
    /// The coordinator's build, for the drift badge.
    pub git_sha: String,
    /// The coordinator's public URL.
    pub public_url: String,
}

/// The coordinator refused this device key (v2 `markBrowserDeviceRejected`,
/// `apps/web/src/store/browser-access.ts:20-29`): the auth-scoped records go,
/// then the gate closes. Only the loss edge acts; a persistently unknown
/// browser does not tear down again on every refresh.
pub fn mark_browser_device_rejected(store: &mut Store, source: &'static str) {
    if store.browser_access_state == BrowserAccessState::Unauthorized {
        return;
    }
    clear_auth_scoped_state(store);
    set_browser_access_state(store, BrowserAccessState::Unauthorized);
    tracing::warn!(target: "store", source, "browser device rejected");
}

/// The protected sessions snapshot published: the one event that grants
/// access (v2 `markProtectedSnapshotPublished`, `browser-access.ts:32-43`).
pub fn mark_protected_snapshot_published(store: &mut Store) {
    set_browser_access_state(store, BrowserAccessState::Authorized);
}
