//! Names of the worker environment entries that travel through an installed
//! service definition (LaunchAgent plist / systemd --user unit). Read by the
//! roost-cli deploy env composer and by the worker that consumes them, so
//! both sides address the same installed entry. Deliberately dependency-free:
//! the worker imports these names on host layouts that do not exist yet, so
//! importing a name must never resolve a path or read a platform.

/// One-shot authorization for retiring a keeper the deployed worker can
/// neither adopt nor prove empty. Spent by the activation that receives it, so
/// an installed value left behind cannot re-authorize discarding live PTYs on
/// every later restart.
pub const KEEPER_FORCE_LIVE_RETIRE_ENV: &str = "ROOST_KEEPER_FORCE_LIVE_RETIRE";

#[cfg(test)]
mod tests {
    use super::KEEPER_FORCE_LIVE_RETIRE_ENV;

    #[test]
    fn the_installed_definition_carries_exactly_these_keys() {
        // A renamed environment entry is invisible at every layer: the
        // installer writes one key, the worker reads another, and the flag
        // reads as ignored rather than as never installed.
        assert_eq!(
            KEEPER_FORCE_LIVE_RETIRE_ENV,
            "ROOST_KEEPER_FORCE_LIVE_RETIRE"
        );
    }
}
