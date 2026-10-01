//! Machine surfaces: what a machine looks like (the identity mark a folder row
//! leads with, its Linux distribution logos, and the static-identity
//! presentation behind both), and how a new one joins — the deploy dialog, its
//! local-access guide, and the enrollment policy both decide with.
//!
//! Ports `apps/web/src/components/machines/` and
//! `apps/web/src/lib/machineIdentity.ts`. The policy half is split the way the
//! product has to reason about it: `enrollment_origin` answers "may another
//! machine dial this?", `deploy_state` answers "what may this dialog do next?",
//! and `deploy_calls` is the only place a coordinator is asked.

pub mod deploy_calls;
pub mod deploy_dialog;
pub mod deploy_state;
pub mod enrollment_origin;
pub mod linux_distribution_mark;
pub mod local_access_guide;
pub mod machine_identity;
pub mod machine_identity_mark;
