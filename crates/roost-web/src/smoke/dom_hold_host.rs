//! Applying the terminal DOM hold to the page: arm it through the pane
//! registry, then check every animation frame whether its renderer or Sync
//! generation retired it. Called by `smoke::dispatch`; the decisions are
//! `smoke::dom_hold`'s. wasm32 only. Ports
//! `apps/web/src/smoke/smokeTerminalDomFault.ts`.

use std::rc::Rc;

use roost_client_core::store::sync_smoke::ready_terminal_generation;

use super::backdoor::SmokeBackdoor;
use super::paint_wait::next_frame;

impl SmokeBackdoor {
    /// `holdTerminalDomForCurrentGeneration`: freeze the pane, then check every
    /// frame whether its renderer or generation retired the hold.
    pub(super) fn hold_terminal_dom(self: &Rc<Self>, session_id: &str) -> Result<(), String> {
        let generation = ready_terminal_generation(self.pump.core().borrow().store());
        let mount_id = self.panes.mount_id(session_id);
        if self
            .holds
            .borrow_mut()
            .release_if_stale(session_id, mount_id, generation.as_ref())
        {
            self.panes.set_dom_hold(session_id, false);
        }
        let hold = self
            .holds
            .borrow_mut()
            .arm(session_id, generation, mount_id)?;
        if !self.panes.set_dom_hold(session_id, true) {
            self.holds.borrow_mut().release(session_id);
            return Err(format!(
                "terminal renderer DOM methods are unavailable: {session_id}"
            ));
        }
        let this = Rc::clone(self);
        let session = session_id.to_owned();
        wasm_bindgen_futures::spawn_local(async move {
            loop {
                next_frame().await;
                if !this.holds.borrow().holds(&session, &hold) {
                    return;
                }
                let mount_id = this.panes.mount_id(&session);
                let generation = ready_terminal_generation(this.pump.core().borrow().store());
                let stale = this.holds.borrow_mut().release_if_stale(
                    &session,
                    mount_id,
                    generation.as_ref(),
                );
                if stale {
                    this.panes.set_dom_hold(&session, false);
                    return;
                }
            }
        });
        Ok(())
    }
}
