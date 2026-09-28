//! Route retirement for the terminal input route owner: a closed browser
//! connection, an ended session, a revoked device, and disposal. Ports the
//! retire/revoke/dispose half of `apps/worker/src/terminal/terminal-input-route-owner.ts`.
//! Called by the downstream dispatch (socket closed), the session-closed hook and
//! shutdown in `runtime::owners`, and the grant-revoke path (W-DOOR).

use super::{
    RouteEntry, RouteStatus, TERMINAL_INPUT_ROUTE_MAX_ENTRIES, TERMINAL_INPUT_ROUTE_TOMBSTONE,
    TerminalInputRouteOwner, prune_retired, route_result, valid_identifier,
};

impl TerminalInputRouteOwner {
    /// Retire every route claimed over a browser connection that closed.
    pub fn retire_connection(&self, connection_id: &str) {
        self.retire_where(connection_id, |entry| {
            entry.actor.connection_id == connection_id
                || entry
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.connection_id == connection_id)
        });
    }

    /// Retire every route into a session that ended.
    pub fn retire_session(&self, session_id: &str) {
        self.retire_where(session_id, |entry| entry.session_id == session_id);
    }

    /// Fence a device out for the life of this owner, retiring its routes.
    pub fn revoke_device(&self, device_fingerprint: &str) {
        if !valid_identifier(device_fingerprint) {
            return;
        }
        {
            let mut state = self.shared.lock();
            if state.revoked_devices.len() >= TERMINAL_INPUT_ROUTE_MAX_ENTRIES {
                state.revoke_overflow = true;
            } else {
                state.revoked_devices.insert(device_fingerprint.to_owned());
            }
        }
        tracing::warn!(
            device_fingerprint,
            "terminal input routes revoked for a device"
        );
        self.retire_where(device_fingerprint, |entry| {
            entry.actor.device_fingerprint == device_fingerprint
        });
    }

    /// Cancel every pending claim and forget every route.
    pub fn dispose(&self) {
        let mut state = self.shared.lock();
        if state.disposed {
            return;
        }
        state.disposed = true;
        for entry in state.routes.values() {
            if let Some(pending) = &entry.pending {
                pending.cancel.request("route_owner_disposed");
            }
        }
        state.routes.clear();
        state.revoked_devices.clear();
        tracing::info!("terminal input route owner disposed");
    }

    fn retire_where(&self, identifier: &str, matches: impl Fn(&RouteEntry) -> bool) {
        if !valid_identifier(identifier) {
            return;
        }
        let now = (self.shared.now)();
        let mut state = self.shared.lock();
        prune_retired(&mut state, now);
        for entry in state.routes.values_mut().filter(|entry| matches(entry)) {
            if let Some(pending) = &entry.pending {
                pending.cancel.request("route_retired");
            }
            entry.input_route_epoch = None;
            entry.status = RouteStatus::Retired;
            entry.retired_until = Some(now + TERMINAL_INPUT_ROUTE_TOMBSTONE);
            let latest = &entry.latest_claim;
            let result = route_result(
                latest,
                &self.shared.worker_epoch,
                false,
                entry.latest_revision,
                "",
                "route_retired",
            );
            entry.latest_result = Some(result);
            tracing::info!(session_id = %entry.session_id, "terminal_input_route_retired");
        }
    }
}
