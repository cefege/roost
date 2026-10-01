//! The one place a notification card hands a store mutation to the client.
//! Every card in this module — a dismissal, a hold, an undo, a raised agent
//! notice — is a `store` mutation the HOST performs, because the trigger is a
//! pointer, a click or a deadline and the client core has no vocabulary for any
//! of the three. `Pump::dispatch` is for events the core folds; these are not
//! events.
//!
//! What repaints the dock is the PUMP's revision `Signal<u64>`, which is read
//! during render by `use_store` and which nothing but the pump writes. The
//! store's own `revision()` counter moves under every write and repaints
//! nothing, so the write has to end at the pump — see `Pump::write_store`,
//! which bumps the signal only when the store's counter actually moved.

use crate::pump::Pump;

use roost_client_core::Store;

/// Run `write` against the client's store, hand back what it produced, and
/// repaint the surfaces subscribed to the store.
pub fn write_store<R>(pump: &Pump, write: impl FnOnce(&mut Store) -> R) -> R {
    pump.write_store(write)
}
