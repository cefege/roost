//! An environment that is the real one with a few variables overridden in
//! memory. Called by the daemon boot resolvers, which turn a command's flags
//! into the same `ROOST_*` names `roost-host` already reads, and by anything
//! else that needs "this flag, else the environment, else the default" without
//! writing to the process.
//!
//! It is in memory on purpose. `roost push` deploys every target in ONE
//! process, so a command that exported a target's identity into the ambient
//! environment would leak one machine's label into the next machine's install —
//! and the coordinator would then list two workers under one name, with the
//! target's real identity gone. Overlaying is how a flag reaches the loader
//! without that happening.

use std::collections::BTreeMap;
use std::path::PathBuf;

use roost_host::EnvSource;

pub struct OverlayEnv<'a> {
    base: &'a dyn EnvSource,
    overrides: BTreeMap<String, String>,
}

/// Hand-written because the wrapped source is a trait object, and a derived
/// `Debug` would demand one of them. What is worth seeing in a failure is which
/// variables THIS invocation overrode, because that is the part that is not the
/// ambient environment.
impl std::fmt::Debug for OverlayEnv<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OverlayEnv")
            .field("overrides", &self.overrides)
            .finish()
    }
}

impl<'a> OverlayEnv<'a> {
    pub fn new(base: &'a dyn EnvSource) -> Self {
        Self {
            base,
            overrides: BTreeMap::new(),
        }
    }

    /// One variable this invocation declares. An empty value is refused rather
    /// than stored: every consumer in `roost-host` treats "set to empty" as
    /// "declared but cleared", so an empty override would silence a variable the
    /// operator did not mean to touch.
    pub fn with(mut self, name: &str, value: Option<&str>) -> Self {
        if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
            self.overrides.insert(name.to_string(), value.to_string());
        }
        self
    }
}

impl EnvSource for OverlayEnv<'_> {
    fn get(&self, key: &str) -> Option<String> {
        self.overrides
            .get(key)
            .cloned()
            .or_else(|| self.base.get(key))
    }

    /// The HOME of the base environment, never an override: a flag that changed
    /// the home directory would move every data directory at once, and no flag
    /// in this crate does that.
    fn home_dir(&self) -> Option<PathBuf> {
        self.base.home_dir()
    }
}

#[cfg(test)]
mod tests {
    // A test's `unwrap` is the assertion: it panics on exactly the value the
    // test says must be there.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::OverlayEnv;
    use roost_host::{EnvSource, MapEnv};

    #[test]
    fn an_override_wins_over_the_process_value() {
        let base = MapEnv::new().with("ROOST_COORDINATOR_BIND", "127.0.0.1:4113");
        let env = OverlayEnv::new(&base).with("ROOST_COORDINATOR_BIND", Some("127.0.0.1:5000"));
        assert_eq!(env.get("ROOST_COORDINATOR_BIND").unwrap(), "127.0.0.1:5000");
    }

    #[test]
    fn a_variable_nobody_overrode_still_comes_from_the_process() {
        let base = MapEnv::new().with("ROOST_COORDINATOR_DB", "/tmp/coord.db");
        let env = OverlayEnv::new(&base).with("ROOST_COORDINATOR_BIND", Some("127.0.0.1:5000"));
        assert_eq!(env.get("ROOST_COORDINATOR_DB").unwrap(), "/tmp/coord.db");
    }

    #[test]
    fn an_empty_override_declares_nothing_rather_than_clearing() {
        // The reason `with` drops empty values: a flag passed as `--bind ""`
        // must not become "the operator declared an empty bind", which every
        // loader in roost-host would read as a deliberate clearing.
        let base = MapEnv::new().with("ROOST_COORDINATOR_BIND", "127.0.0.1:4113");
        let env = OverlayEnv::new(&base).with("ROOST_COORDINATOR_BIND", Some("   "));
        assert_eq!(env.get("ROOST_COORDINATOR_BIND").unwrap(), "127.0.0.1:4113");
    }

    #[test]
    fn the_base_home_directory_is_never_overridden() {
        let base = MapEnv::new().with("HOME", "/home/op");
        let env = OverlayEnv::new(&base).with("HOME", Some("/tmp"));
        assert_eq!(
            env.home_dir().unwrap(),
            std::path::PathBuf::from("/home/op")
        );
    }
}
