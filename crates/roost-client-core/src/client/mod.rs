//! The client's wire services: the Connect client, the Sync dispatch, the
//! acknowledged UI commands, and the state each service keeps.
//!
//! One module per service, and each one owns the vocabulary its callers speak.
//! `rpc` shapes a Connect call and decides what happens when the credential
//! cannot be minted; `sync` places every frame a socket delivers and reads a
//! close code; `carriers` elects the direct transport a session rides and
//! `local` opens the loopback door that is preferred over it; `attachments`
//! moves files over whichever carrier won; `predictive_echo` owns the characters
//! a fast typist sees before the PTY answers. A service that restated a type
//! another service already owns is how two clients end up disagreeing about the
//! same arrangement.
//!
//! None of them is the state machine. `ClientCore` owns the generations, the
//! domains, the readiness gate and the acknowledgements; these sit on the host
//! side of that boundary and hand their answers in as `ClientEvent`s.

pub mod agents;
pub mod attachments;
pub mod auth;
pub mod carriers;
pub mod global_search;
pub mod local;
pub mod predictive_echo;
pub mod rpc;
pub mod sync;
pub mod ui_state;
