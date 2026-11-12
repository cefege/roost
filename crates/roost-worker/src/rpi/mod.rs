//! Worker-owned Rpi configuration, persistence and harness integration.
//! The operator CLI shares provider setup; coordinator traffic uses typed ports.
//! Provider credentials and execution state stay below the worker data directory.

pub mod provider;
