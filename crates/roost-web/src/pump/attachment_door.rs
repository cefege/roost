//! The local worker door an attachment upload may open its loopback socket
//! against: the one this page's door discovery adopted.
//!
//! Called by `components::terminal_chrome::upload_host` at upload time, which
//! is when v2 reads it (`readLocalWorkerDoor()` in `attachmentDirect.ts`).
//! Depends on `super::carriers`, which owns discovery; this only reads it.

use roost_client_core::client::local::discovery::LocalWorkerDoor;

use super::Pump;

impl Pump {
    /// The door discovery has adopted, once it has answered.
    pub fn local_worker_door(&self) -> Option<LocalWorkerDoor> {
        #[cfg(target_arch = "wasm32")]
        {
            self.inner.carriers.borrow().door().cloned()
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            None
        }
    }
}
