//! Smoke-only renderer fault arms: freeze a pane's DOM while its replica keeps
//! folding, and drop exactly one replica→renderer delivery. Compiled only with
//! the `smoke` feature; armed by `crate::smoke`, consumed by the pane mount's
//! paint loop and delivery. Ports the fault halves of
//! `apps/web/src/smoke/smokeTerminalDomFault.ts` and
//! `apps/web/src/store/terminal-stream-replica.ts` (`suppressNextRendererFrame`).

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use super::pane_registry::PaneRegistry;

/// The armed faults, keyed by session.
#[derive(Default)]
pub struct PaneFaults {
    dom_held: BTreeSet<String>,
    drop_armed: BTreeSet<String>,
    dropped: BTreeMap<String, u64>,
    drop_taken_hook: Option<Rc<dyn Fn(&str)>>,
}

impl std::fmt::Debug for PaneFaults {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PaneFaults")
            .field("dom_held", &self.dom_held)
            .field("drop_armed", &self.drop_armed)
            .field("dropped", &self.dropped)
            .finish_non_exhaustive()
    }
}

impl PaneRegistry {
    /// Freeze a mounted pane's DOM (false, arming nothing, when none is
    /// mounted), or release a hold: a release always removes it, mounted or
    /// not, so the session's next mount never comes up frozen.
    pub fn set_dom_hold(&self, session_id: &str, held: bool) -> bool {
        self.with_faults(
            |faults, mounted| {
                if !held {
                    return faults.dom_held.remove(session_id);
                }
                mounted && faults.dom_held.insert(session_id.to_owned())
            },
            session_id,
        )
    }

    /// Whether the session's pane must not paint.
    pub fn dom_held(&self, session_id: &str) -> bool {
        self.with_faults(|faults, _| faults.dom_held.contains(session_id), session_id)
    }

    /// Arm a drop of the session's next renderer delivery. Accepted before any
    /// pane is mounted: the first delivery once one is takes it.
    pub fn arm_drop_next_frame(&self, session_id: &str) {
        self.with_faults(
            |faults, _| faults.drop_armed.insert(session_id.to_owned()),
            session_id,
        );
        tracing::info!(target: "smoke", session_id, "renderer frame drop armed");
    }

    /// How many deliveries an arm has dropped for the session.
    pub fn dropped_frame_count(&self, session_id: &str) -> u64 {
        self.with_faults(
            |faults, _| faults.dropped.get(session_id).copied().unwrap_or(0),
            session_id,
        )
    }

    /// Called with the session id whenever an arm is consumed, so the owner of
    /// a persisted arm can retire it.
    pub fn set_frame_drop_taken_hook(&self, hook: Rc<dyn Fn(&str)>) {
        self.with_faults(|faults, _| faults.drop_taken_hook = Some(hook), "");
    }

    /// Consume an armed drop for a mounted pane's delivery. True means the
    /// caller skips exactly this delivery.
    pub fn take_frame_drop(&self, session_id: &str) -> bool {
        let taken = self.with_faults(
            |faults, mounted| {
                if !mounted || !faults.drop_armed.remove(session_id) {
                    return None;
                }
                *faults.dropped.entry(session_id.to_owned()).or_default() += 1;
                Some(faults.drop_taken_hook.clone())
            },
            session_id,
        );
        let Some(hook) = taken else {
            return false;
        };
        tracing::info!(target: "smoke", session_id, "renderer frame delivery dropped");
        if let Some(hook) = hook {
            hook(session_id);
        }
        true
    }
}
