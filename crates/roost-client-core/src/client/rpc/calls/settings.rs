//! The coordinator methods the Settings panes call directly, one submodule per
//! domain, each request a `unary::UnaryMethod` with Rust-typed answers.
//!
//! Only methods a v2 settings pane actually reaches for are here. A coordinator
//! method with no v2 control behind it — the auth dashboard, federation, and
//! credential-relocation families — is deliberately absent: a pane that invents
//! a button for a method the product never exposed would be a control nobody
//! tested. Ports `apps/web/src/components/Settings/*.tsx` and the `coordClient`
//! calls they make.

pub mod agent_config;
pub mod attachments;
pub mod audit;
pub mod bootstrap;
pub mod devices;
pub mod machines;
pub mod mcp;
pub mod metrics;
pub mod push;
pub mod transcription;
