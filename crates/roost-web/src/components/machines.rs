//! Machine surfaces: what a machine looks like (the identity mark a folder row
//! leads with, its Linux distribution logos, and the static-identity
//! presentation behind both), and how a new one joins — the deploy dialog, its
//! local-access guide, and the enrollment policy both decide with.
//!
//! Enrollment origin policy belongs to client-core's access model;
//! `deploy_state` decides what the dialog may do next and `deploy_calls`
//! asks the coordinator for the current declaration.

pub mod deploy_calls;
pub mod deploy_dialog;
pub mod deploy_state;
pub mod host_usage;
pub mod linux_distribution_mark;
pub mod local_access_guide;
pub mod machine_identity;
pub mod machine_identity_mark;
