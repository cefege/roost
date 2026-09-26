// The two things every coordinator domain handler is handed, and nothing else.
//
// A handler's whole world is `&CoordCore` for process state plus `&Caller` for
// who is asking. Keeping both behind one module means a domain slice can be
// written, reviewed and tested without reading the service impl, and adding a
// per-process singleton does not widen the one file every domain also edits.

pub mod caller;
pub mod core;

pub use caller::{Caller, ListenerTrust};
pub use core::CoordCore;
