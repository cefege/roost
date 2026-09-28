//! The terminal DOM hold's decisions: when a hold may be armed, when an armed
//! one is stale (its renderer retired or its Sync generation moved), and when a
//! second arm is refused. Native; `smoke::dispatch` applies them to the pane
//! registry's `set_dom_hold` and checks retirement every animation frame. Ports
//! `apps/web/src/smoke/smokeTerminalDomFault.ts`.

use std::collections::BTreeMap;

use roost_client_core::TerminalToken;

/// One armed hold: the renderer mount and the generation it was armed under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomHold {
    pub mount_id: u64,
    pub generation: TerminalToken,
}

/// Every armed hold, by session.
#[derive(Debug, Default)]
pub struct DomHolds {
    holds: BTreeMap<String, DomHold>,
}

impl DomHolds {
    /// Arm a hold on `session_id` for the renderer `mount_id` under the ready
    /// `generation`, with v2's refusals. The caller releases a stale hold first
    /// (`release_if_stale`), so a hold still present here is a live one.
    /// Answers the armed hold, which the retirement check compares against.
    pub fn arm(
        &mut self,
        session_id: &str,
        generation: Option<TerminalToken>,
        mount_id: Option<u64>,
    ) -> Result<DomHold, String> {
        let Some(generation) = generation else {
            return Err(format!(
                "terminal DOM hold requires a ready terminal generation: {session_id}"
            ));
        };
        if self.holds.contains_key(session_id) {
            return Err(format!("terminal DOM hold already active for {session_id}"));
        }
        let Some(mount_id) = mount_id else {
            return Err(format!(
                "terminal DOM hold requires a registered renderer: {session_id}"
            ));
        };
        let hold = DomHold { mount_id, generation };
        self.holds.insert(session_id.to_owned(), hold.clone());
        tracing::info!(target: "smoke", session_id, mount_id, "terminal DOM hold armed");
        Ok(hold)
    }

    /// Drop `session_id`'s hold when its renderer was replaced or unmounted, or
    /// its Sync generation moved on. `true` means the caller restores the pane.
    pub fn release_if_stale(
        &mut self,
        session_id: &str,
        mount_id: Option<u64>,
        generation: Option<&TerminalToken>,
    ) -> bool {
        let stale = self.holds.get(session_id).is_some_and(|hold| {
            Some(hold.mount_id) != mount_id || Some(&hold.generation) != generation
        });
        stale && self.release(session_id)
    }

    /// Whether `session_id` still holds exactly `hold`.
    pub fn holds(&self, session_id: &str, hold: &DomHold) -> bool {
        self.holds.get(session_id) == Some(hold)
    }

    /// Release a session's hold; `true` when there was one.
    pub fn release(&mut self, session_id: &str) -> bool {
        let released = self.holds.remove(session_id).is_some();
        if released {
            tracing::info!(target: "smoke", session_id, "terminal DOM hold released");
        }
        released
    }
}
