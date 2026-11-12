//! Worker-local Rpi conversation command admission and device RPCs.
//! Shared conversation rows retain backend identity; commands persist before
//! delivery and model catalogs stay on the authenticated owning worker.

mod commands;
pub mod rpc;
