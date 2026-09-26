//! The worker's terminal sessions: the record every slice keys on, the ring it
//! retains bytes in, the history and agent evidence it carries, and the two
//! traits that deliver into it. `runtime::serve` owns the manager that holds
//! them; `session::spawn`, `session::lifecycle` and `session::emit` own its
//! transitions. Depends on `roost_term` for the core, `event_store` for the
//! durable claim, and `crate::shell_spec` for the launch contract — and nothing
//! here depends on any of them back.

pub mod agent_osc;
pub mod history;
pub mod ring;
pub mod sinks;
pub mod types;
