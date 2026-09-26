//! Installed-service machinery: one definition of what a Roost service is, the
//! two formats the supported platforms read that definition in, the install
//! that puts a release and its definition on disk, and the journaled deploy
//! that can put the previous definition back byte for byte.
//!
//! `roost-host` owns the names — labels, definition paths, data and log
//! directories — and `status/service_definition.rs` owns the reader for what is
//! installed. This module owns the writer and every change to what is on disk,
//! so `quickstart`, `deploy` and `push` share one implementation instead of
//! three drifting copies. Nothing here parses a command line.

pub mod atomic_file;
pub mod definition_text;
pub mod deploy_journal;
pub mod deploy_transaction;
pub mod install;
pub mod launchd_plist;
pub mod memory_limits;
pub mod service_argv;
pub mod service_control;
pub mod service_environment;
pub mod service_settings;
pub mod service_spec;
pub mod systemd_syntax;
pub mod systemd_unit;
